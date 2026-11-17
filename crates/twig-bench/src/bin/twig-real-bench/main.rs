#[cfg(unix)]
mod unix;

fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        unix::run()
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("twig-real-bench requires macOS or Linux");
    }
}
