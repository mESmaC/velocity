use std::path::PathBuf;
use velocity_airc::{Channel, Error, serve};

fn main() -> Result<(), Error> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 4 {
        return Err(Error::Invalid(
            "Usage: velocity-airc <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick>"
                .into(),
        ));
    }
    let project = arguments[1]
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid project identity".into()))?;
    let member = arguments[2]
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid member identity".into()))?;
    let nick = arguments[3]
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid nick".into()))?;
    let channel = Channel::open(&PathBuf::from(&arguments[0]), project)?;
    serve(
        &channel,
        member,
        nick,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}
