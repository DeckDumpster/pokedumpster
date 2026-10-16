//! The container tier's Access issuer: the kit's `TestAccessFixture` (a real RS256 key and JWKS
//! endpoint) made reachable from a container, for `tests/tenants/handles.sh`.
//!
//! Usage: `access_fixture <outdir> <email>...`. Writes `<outdir>/fixture.env` — `JWKS_PORT`,
//! `AUD`, `ISSUER` and one `TOKEN_<n>` per email, in argument order — and then serves until
//! killed. The fixture's own listener is loopback-only, so a forwarder on `0.0.0.0` stands in
//! front of it; the app container reaches that through `host.containers.internal`.

use dumpster_kit_tenancy::testing::TestAccessFixture;
use std::fmt::Write as _;
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .expect("usage: access_fixture <outdir> <email>...");
    let emails: Vec<String> = args.collect();

    let fx = TestAccessFixture::new().await;
    let upstream = fx
        .jwks_url
        .strip_prefix("http://")
        .and_then(|r| r.split('/').next())
        .expect("fixture jwks_url is http://host:port/…")
        .to_string();

    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut inbound, _) = listener.accept().await.unwrap();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let mut outbound = TcpStream::connect(&upstream).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });

    let mut env = String::new();
    writeln!(env, "JWKS_PORT={port}").unwrap();
    writeln!(env, "AUD={}", fx.aud).unwrap();
    writeln!(env, "ISSUER={}", fx.team_domain).unwrap();
    for (i, email) in emails.iter().enumerate() {
        writeln!(env, "TOKEN_{i}={}", fx.valid_token(email)).unwrap();
    }
    // Written last and renamed into place: the reader treats the file's existence as "ready".
    let tmp = format!("{out}/fixture.env.tmp");
    std::fs::write(&tmp, env).unwrap();
    std::fs::rename(&tmp, format!("{out}/fixture.env")).unwrap();

    std::future::pending::<()>().await;
}
