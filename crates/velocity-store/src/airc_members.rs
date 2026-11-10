//! What the AIRC modal's nick list and claims board know about each channel member
//! (F14), read from saved thread and duty state.

use velocity_core::airc_modal::{AircMember, AircMemberState};

use crate::project_layout::thread_metadata;
use crate::{ProjectItem, ThreadItem, ThreadState};

/// Duty agents join as `duty-<nick>` and threads as `thread-<id>` (F14 runtime injection).
pub fn airc_members(project: &ProjectItem) -> Vec<AircMember> {
    let duty = project.duty_roles.iter().flat_map(|role| {
        role.agents.iter().map(move |agent| {
            AircMember::new(
                &format!("duty-{}", agent.nickname),
                AircMemberState::Duty {
                    nick: agent.nickname.clone(),
                    detail: if role.paused {
                        "paused".into()
                    } else {
                        agent.doing.clone()
                    },
                    idle: role.paused || !agent.busy,
                },
            )
        })
    });
    let threads = project.threads.iter().filter_map(|thread| {
        thread_state(thread).map(|state| AircMember::new(&format!("thread-{}", thread.id), state))
    });
    duty.chain(threads).collect()
}

fn thread_state(thread: &ThreadItem) -> Option<AircMemberState> {
    let number = thread.pull_request_number;
    Some(match thread.state {
        ThreadState::Working
            if thread
                .goal_member
                .as_ref()
                .is_some_and(|member| member.choosing) =>
        {
            AircMemberState::Choosing
        }
        ThreadState::Working => AircMemberState::Working {
            title: thread.title.clone(),
        },
        ThreadState::Review => AircMemberState::Review {
            number,
            label: thread_metadata(thread),
        },
        ThreadState::Error => AircMemberState::Failed,
        ThreadState::Closed => AircMemberState::Closed { number },
        ThreadState::Merged => AircMemberState::Merged { number },
        ThreadState::Draft | ThreadState::Spec | ThreadState::Done => return None,
    })
}
