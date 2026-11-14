//! F7 "Checks before merge" without a window: prints what Velocity would do now with a PR's
//! head if it were approved. Reads GitHub only; nothing is approved or merged.
//!
//! cargo run -p velocity-github --example merge_gate -- <checkout> <pr number>...
use std::path::Path;

use velocity_github::PullRequestClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [checkout, numbers @ ..] = arguments.as_slice() else {
        return Err("Usage: merge_gate <checkout> <pr number>...".into());
    };
    let client = PullRequestClient::discover(Path::new(checkout))?;
    for number in numbers {
        let number: u32 = number.parse()?;
        let head = client.get(number)?.head_ref_oid;
        println!(
            "#{number} at {}: has checks {}, with checks required {:?}, on approval {:?}",
            head.get(..7).unwrap_or(&head),
            client.has_checks(number)?,
            client.landing(number, &head, true)?,
            client.landing(number, &head, false)?,
        );
    }
    Ok(())
}
