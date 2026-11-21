//! Native F14 message edge: queued input, persistent acknowledgement and user comments.
use std::collections::{BTreeMap, BTreeSet};

use tauri::{AppHandle, Manager};
use velocity_airc::Channel;
use velocity_rosen::EventHead;
use velocity_store::{ProjectItem, ThreadItem};
use velocity_threads::DeliveryStatus;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

fn channel(application: &AppHandle, project: u32) -> Result<Channel, String> {
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    Channel::open(&directory, &project.to_string()).map_err(|error| error.to_string())
}

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        let mut left = Left::new();
        let mut members = Members::new();
        let mut pass: u64 = 0;
        loop {
            pass = pass.wrapping_add(1);
            // A member can rejoin without this loop seeing its thread live (a resume
            // that fails between passes), so the set is cleared now and then: a missed
            // leave is at most this many seconds late.
            if pass.is_multiple_of(LEFT_RECHECK_PASSES) {
                left.clear();
            }
            if let Err(error) = cycle(&application, &mut left, &mut members) {
                eprintln!("Velocity AIRC delivery: {error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// Threads whose member this loop has already taken out of the channel. A stopped thread
/// leaves once, not every second: each leave locks and reads the channel (#322).
type Left = BTreeSet<velocity_threads::ThreadKey>;
/// Each native join is retained only for its current agent session.
type Members = BTreeMap<velocity_threads::ThreadKey, Member>;

struct Member {
    session: String,
    nick: String,
    server_identity: Option<String>,
}

const LEFT_RECHECK_PASSES: u64 = 30;

fn cycle(application: &AppHandle, left: &mut Left, members: &mut Members) -> Result<(), String> {
    // Transcripts are shared, not copied, so this clone copies thread records only.
    let projects = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .clone();
    members.retain(|(project, thread), _| {
        projects
            .iter()
            .any(|item| item.id == *project && item.threads.iter().any(|item| item.id == *thread))
    });
    for project in projects {
        if let Err(error) = project_cycle(application, &project, left, members) {
            eprintln!("Velocity AIRC project {}: {error}", project.id);
        }
    }
    Ok(())
}

fn project_cycle(
    application: &AppHandle,
    project: &ProjectItem,
    left: &mut Left,
    members: &mut Members,
) -> Result<(), String> {
    // One handle for the whole pass: opening resolves and creates the folder each time.
    let channel = channel(application, project.id)?;
    let changed = velocity_threads::agent_tasks::ingest_shared(
        application.state::<ManagedStateStore>().inner(),
        project.id,
        &channel,
    )
    .map_err(|error| error.to_string())?;
    if changed {
        crate::threads::refresh(
            application,
            (project.id, project.active_thread_id.unwrap_or(0)),
            false,
        )?;
    }
    if let Err(error) = update_waits(application, project, &channel) {
        eprintln!("Velocity AIRC project {} waits: {error}", project.id);
    }
    crate::agent_activity::cycle(application, project, &channel)?;
    crate::parking::cycle(application, project, &channel);
    let head = channel
        .server_event_head()
        .map_err(|error| error.to_string())?;
    for thread in &project.threads {
        if let Err(error) = deliver(
            application,
            (project.id, thread),
            &channel,
            head.as_ref(),
            left,
            members,
        ) {
            eprintln!("Velocity AIRC thread {}: {error}", thread.id);
        }
    }
    Ok(())
}

fn deliver(
    application: &AppHandle,
    (project, thread): (u32, &ThreadItem),
    channel: &Channel,
    head: Option<&EventHead>,
    left: &mut Left,
    members: &mut Members,
) -> Result<(), String> {
    let key = (project, thread.id);
    let member = thread.airc_member();
    let (live, setting_up, session) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (
            runtimes.is_live(key),
            runtimes.is_setting_up(key),
            runtimes.session_identifier(key),
        )
    };
    if !live {
        members.remove(&key);
        if left.contains(&key) {
            return Ok(());
        }
        channel.leave(&member).map_err(|error| error.to_string())?;
        left.insert(key);
        return Ok(());
    }
    left.remove(&key);
    if setting_up {
        return Ok(());
    }
    if thread.worktree_path.is_none() {
        return Ok(());
    }
    let session = session.ok_or("Missing live AIRC session")?;
    let identity = head.map(|head| head.identity.as_str());
    let requested_nick = thread.agent_name();
    let nick = member_nick(
        channel,
        key,
        &member,
        &requested_nick,
        &session,
        identity,
        members,
    )?;
    deliver_pending(application, key, thread, channel, &nick, head)
}

fn member_nick(
    channel: &Channel,
    key: velocity_threads::ThreadKey,
    member: &str,
    requested_nick: &str,
    session: &str,
    server_identity: Option<&str>,
    members: &mut Members,
) -> Result<String, String> {
    if let Some(known) = members.get(&key)
        && known.session == session
        && known.server_identity.as_deref() == server_identity
    {
        return Ok(known.nick.clone());
    }
    let nick = channel
        .join(member, requested_nick)
        .map_err(|error| error.to_string())?;
    members.insert(
        key,
        Member {
            session: session.to_owned(),
            nick: nick.clone(),
            server_identity: server_identity.map(str::to_owned),
        },
    );
    Ok(nick)
}

fn deliver_pending(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
    thread: &ThreadItem,
    channel: &Channel,
    nick: &str,
    head: Option<&EventHead>,
) -> Result<(), String> {
    let identity = head.map(|head| head.identity.as_str());
    let identity_changed = thread.airc_server_identity.as_deref() != identity;
    let past_end = head.is_some_and(|head| thread.airc_cursor > head.latest);
    let rewound = past_end
        || (identity_changed && (thread.airc_cursor != 0 || thread.airc_server_identity.is_some()));
    let after = if rewound {
        0
    } else if thread.airc_server_identity.is_some() {
        thread.airc_cursor
    } else {
        channel
            .delivery_cursor(&thread.airc_member(), thread.airc_cursor)
            .map_err(|error| error.to_string())?
    };
    let cursor = velocity_airc::deliver_targeted(
        channel,
        nick,
        after,
        thread.activity == velocity_store::AgentActivity::Idle,
        |delivery| {
            let status = lock(
                application.state::<ManagedThreads>().inner(),
                "thread sessions",
            )?
            .deliver_airc(key, &delivery.clone().with_server_identity(identity))
            .map_err(|error| error.to_string())?;
            Ok(status == DeliveryStatus::Delivered)
        },
    )
    .map_err(|error| error.to_string())?;
    if cursor != thread.airc_cursor || identity_changed {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .update_thread(key.0, key.1, |saved| {
            saved.airc_cursor = cursor;
            saved.airc_server_identity = identity.map(str::to_owned);
        })
        .map_err(|error| error.to_string())?;
        if rewound {
            let (project, thread_id) = key;
            let previous = thread.airc_cursor;
            eprintln!(
                "Velocity AIRC project {project} thread {thread_id}: restarting at 0 (saved {previous}, database changed: {identity_changed}, past latest: {past_end})"
            );
        }
    }
    Ok(())
}

/// Translate the runner's saved lifecycle into wake events. The waiting MCP server
/// subscribes to these writes; it never polls GitHub or the runner.
fn update_waits(
    application: &AppHandle,
    project: &ProjectItem,
    channel: &Channel,
) -> Result<(), String> {
    for (identifier, description) in velocity_threads::dependencies::refresh(channel, project)
        .map_err(|error| error.to_string())?
    {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .update_thread(project.id, identifier, |saved| {
            saved.dependency_wait = description
        })
        .map_err(|error| error.to_string())?;
        crate::threads::refresh(application, (project.id, identifier), false)?;
    }
    Ok(())
}

/// Sending is independent of the later modal: the typed IPC is also usable headlessly.
#[tauri::command]
#[specta::specta]
pub async fn airc_send(
    application: AppHandle,
    project_identifier: u32,
    text: String,
) -> Result<(), String> {
    crate::layout::off_ui_thread(move || {
        let exists = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .state()
        .projects
        .iter()
        .any(|project| project.id == project_identifier);
        if !exists {
            return Err("Unknown project".into());
        }
        channel(&application, project_identifier)?
            .comment(&text)
            .map_err(|error| error.to_string())?;
        crate::airc_modal::refresh(&application)
    })
    .await
}
