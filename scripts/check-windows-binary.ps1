# Refuse a Windows binary that needs the Visual C++ Redistributable, which a
# clean Windows install lacks, and check that it starts.
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = "Stop"
$text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Path))
foreach ($runtime in "VCRUNTIME140.dll", "MSVCP140.dll") {
  if ($text.IndexOf($runtime, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw "$Path imports $runtime; link the C runtime statically (.cargo/config.toml)"
  }
}
$version = & $Path --version
if ($LASTEXITCODE -ne 0 -or $version -notmatch '^git-twig \d+\.\d+\.\d+') { throw "$Path --version failed: $version" }
"$version runs without the Visual C++ Redistributable"
