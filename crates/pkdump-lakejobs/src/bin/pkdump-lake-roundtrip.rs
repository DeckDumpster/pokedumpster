//! `pkdump-lake-roundtrip` — write, read, commit again, and time-travel back.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pkdump_lakejobs::roundtrip::run().await
}
