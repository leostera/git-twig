#[cfg(unix)]
mod unix;

fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        unix::main()
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("twig-profile requires macOS");
    }
}
