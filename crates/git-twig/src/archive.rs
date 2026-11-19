//! Git's own checkout bytes for converted files, read from `git archive`.
//!
//! `git archive` writes each file through the same working-tree conversion as
//! checkout: end-of-line conversion, `ident` and `working-tree-encoding`, with
//! attributes from the archived tree. One stream holds the bytes stock Git
//! would write for every converted path, so a donor clone is certified only by
//! equality with what Git itself produced. `export-subst` and `export-ignore`
//! make archives differ from checkouts; those paths are never compared.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BLOCK: usize = 512;

/// Compare each wanted private file with the bytes Git's archive holds for its
/// path. Returns one verdict per wanted path; an absent path is unequal.
pub fn compare(mut command: Command, data: &Path, wanted: &[&Path]) -> Result<Vec<bool>> {
    let mut verdicts = vec![false; wanted.len()];
    if wanted.is_empty() {
        return Ok(verdicts);
    }
    let positions: HashMap<&Path, usize> = wanted
        .iter()
        .enumerate()
        .map(|(position, path)| (*path, position))
        .collect();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().context("missing archive stdout")?;
    let mut stream = BufReader::with_capacity(1 << 20, stdout);
    let result = (|| -> Result<()> {
        let mut pending_path = None;
        let mut pending_size = None;
        let mut header = [0u8; BLOCK];
        loop {
            stream.read_exact(&mut header)?;
            if header.iter().all(|byte| *byte == 0) {
                return Ok(()); // End of archive; Git pads with zero blocks.
            }
            ensure!(checksum_matches(&header), "corrupt archive header");
            let kind = header[156];
            let mut size = match pending_size.take() {
                Some(size) => size,
                None => octal(&header[124..136])?,
            };
            match kind {
                b'g' | b'x' => {
                    let records = read_payload(&mut stream, size)?;
                    if kind == b'x' {
                        for (key, value) in pax_records(&records)? {
                            match key {
                                b"path" => pending_path = Some(value.to_vec()),
                                b"size" => {
                                    pending_size = Some(std::str::from_utf8(value)?.parse()?)
                                }
                                _ => {}
                            }
                        }
                    }
                    continue;
                }
                b'0' | 0 => {}
                _ => {
                    // Directories, symlinks and gitlinks carry no file bytes.
                    pending_path = None;
                    skip(&mut stream, size, size)?;
                    continue;
                }
            }
            let name = match pending_path.take() {
                Some(path) => path,
                None => ustar_name(&header),
            };
            let path = PathBuf::from(crate::native::os_string(name));
            let total = size;
            let Some(&position) = positions.get(path.as_path()) else {
                skip(&mut stream, size, total)?;
                continue;
            };
            verdicts[position] = same_bytes(&mut stream, &data.join(&path), &mut size)?;
            skip(&mut stream, size, total)?;
        }
    })();
    // Drain so Git never blocks on a full pipe, then require its success.
    let _ = std::io::copy(&mut stream, &mut std::io::sink());
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait()?;
    result?;
    ensure!(status.success(), "git archive failed: {stderr}");
    Ok(verdicts)
}

/// Consume up to `size` archived bytes while comparing them with the file.
/// Leaves in `size` what remains unread of the entry.
fn same_bytes(stream: &mut impl Read, path: &Path, size: &mut u64) -> Result<bool> {
    let mut file = match open_regular(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    if file.metadata()?.len() != *size {
        return Ok(false);
    }
    let mut archived = vec![0u8; 1 << 16];
    let mut private = vec![0u8; 1 << 16];
    while *size > 0 {
        let count = (*size).min(archived.len() as u64) as usize;
        stream.read_exact(&mut archived[..count])?;
        *size -= count as u64;
        file.read_exact(&mut private[..count])?;
        if archived[..count] != private[..count] {
            return Ok(false);
        }
    }
    Ok(file.read(&mut private[..1])? == 0)
}

/// Private candidates are owned staging files; never follow a link there.
fn open_regular(path: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    ensure!(file.metadata()?.is_file(), "not a regular file");
    Ok(file)
}

/// Skip what remains of an entry of `total` bytes, and its padding.
fn skip(stream: &mut impl Read, remaining: u64, total: u64) -> Result<()> {
    let padded = remaining + padding_after(total);
    let copied = std::io::copy(&mut stream.take(padded), &mut std::io::sink())?;
    ensure!(copied == padded, "truncated archive");
    Ok(())
}

/// Bytes of padding after an entry: each one ends on a block boundary.
fn padding_after(total: u64) -> u64 {
    (BLOCK as u64 - total % BLOCK as u64) % BLOCK as u64
}

fn read_payload(stream: &mut impl Read, size: u64) -> Result<Vec<u8>> {
    ensure!(size < 1 << 20, "oversized archive extension header");
    let mut payload = vec![0u8; size as usize];
    stream.read_exact(&mut payload)?;
    skip(stream, 0, size)?;
    Ok(payload)
}

/// POSIX pax records: `<length> <key>=<value>\n`, where length counts the
/// whole record.
fn pax_records(mut bytes: &[u8]) -> Result<Vec<(&[u8], &[u8])>> {
    let mut records = Vec::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .context("malformed pax record")?;
        let length: usize = std::str::from_utf8(&bytes[..space])?.parse()?;
        ensure!(
            length > space + 1 && length <= bytes.len() && bytes[length - 1] == b'\n',
            "malformed pax record"
        );
        let record = &bytes[space + 1..length - 1];
        let equals = record
            .iter()
            .position(|byte| *byte == b'=')
            .context("malformed pax record")?;
        records.push((&record[..equals], &record[equals + 1..]));
        bytes = &bytes[length..];
    }
    Ok(records)
}

fn ustar_name(header: &[u8; BLOCK]) -> Vec<u8> {
    let field = |range: std::ops::Range<usize>| -> Vec<u8> {
        let bytes = &header[range];
        bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())].to_vec()
    };
    let name = field(0..100);
    let prefix = if &header[257..262] == b"ustar" {
        field(345..500)
    } else {
        Vec::new()
    };
    if prefix.is_empty() {
        name
    } else {
        [prefix, b"/".to_vec(), name].concat()
    }
}

fn octal(field: &[u8]) -> Result<u64> {
    let digits: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|byte| *byte == b' ')
        .take_while(|byte| (b'0'..=b'7').contains(byte))
        .collect();
    if digits.is_empty() {
        bail!("malformed archive number");
    }
    Ok(u64::from_str_radix(std::str::from_utf8(&digits)?, 8)?)
}

fn checksum_matches(header: &[u8; BLOCK]) -> bool {
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum();
    octal(&header[148..156]).is_ok_and(|recorded| recorded == sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_records_carry_long_paths_and_sizes() -> Result<()> {
        let records = pax_records(b"12 path=a/b\n11 size=42\n")?;
        assert_eq!(
            records,
            vec![(&b"path"[..], &b"a/b"[..]), (&b"size"[..], &b"42"[..])]
        );
        assert!(pax_records(b"99 path=a\n").is_err());
        assert!(pax_records(b"5 pa\n").is_err());
        Ok(())
    }

    #[test]
    fn padding_ends_entries_on_block_boundaries() {
        assert_eq!(padding_after(0), 0);
        assert_eq!(padding_after(1), 511);
        assert_eq!(padding_after(512), 0);
        assert_eq!(padding_after(513), 511);
    }
}
