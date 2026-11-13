//! Exercise F8 create, read, update and delete through the real AIRC MCP server and the
//! store owner's request drain, the way an agent session reaches them.
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use velocity_airc::Channel;
use velocity_store::threads::NewThread;
use velocity_store::{PersistedState, ProjectItem, StateStore};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: task_crud <new scratch directory>")?;
    seed(&directory)?;
    let mut store = StateStore::load_or_default(&directory)?;
    let thread = store.add_thread(
        1,
        NewThread::new(
            "Author".into(),
            "vel/author".into(),
            directory.clone(),
            &store.settings(),
        ),
    )?;
    let store = Arc::new(Mutex::new(store));
    let channel = Channel::open(std::path::Path::new(&directory), "1")?;
    let finished = Arc::new(AtomicBool::new(false));
    let owner_channel = Channel::open(std::path::Path::new(&directory), "1")?;
    let owner = run_store_owner(store.clone(), owner_channel, finished.clone());
    let output = Output::default();
    let requests = requests().join("\n") + "\n";
    velocity_airc::serve(
        &channel,
        &format!("thread-{thread}"),
        "author",
        std::io::Cursor::new(requests.into_bytes()),
        output.clone(),
    )?;
    finished.store(true, Ordering::Release);
    owner.join().map_err(|_| "store owner panicked")?;
    let responses = String::from_utf8(output.0.lock().map_err(|_| "poisoned")?.clone())?;
    for line in responses.lines().skip(2) {
        let response: serde_json::Value = serde_json::from_str(line)?;
        println!("{}", response["result"]["content"][0]["text"]);
    }
    let store = StateStore::load_or_default(&directory)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.project(1).ok_or("lost")?.tasks)?
    );
    for event in channel.events(0)? {
        println!("{} {}", event.nick, event.text);
    }
    Ok(())
}

fn run_store_owner(
    store: Arc<Mutex<StateStore>>,
    channel: Channel,
    finished: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !finished.load(Ordering::Acquire) {
            if let Ok(mut store) = store.lock()
                && let Err(error) = velocity_threads::agent_tasks::ingest(&mut store, 1, &channel)
            {
                eprintln!("ingest: {error}");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    })
}

fn requests() -> Vec<String> {
    let call = |id: u32, name: &str, arguments: serde_json::Value| {
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}})
        .to_string()
    };
    vec![
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string(),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
        call(3, "task_add", serde_json::json!({"title":"write the docs"})),
        call(4, "task_add", serde_json::json!({"title":"Ship it"})),
        call(5, "task_list", serde_json::json!({})),
        call(
            6,
            "task_update",
            serde_json::json!({"task":1,"title":"Write the user docs","state":"done"}),
        ),
        call(7, "task_delete", serde_json::json!({"task":2})),
        call(
            8,
            "task_update",
            serde_json::json!({"task":9,"state":"todo"}),
        ),
        call(
            9,
            "task_update",
            serde_json::json!({"task":1,"state":"todo"}),
        ),
        call(10, "task_list", serde_json::json!({})),
    ]
}

fn seed(directory: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir(directory)?;
    let mut state = PersistedState::default();
    let mut project = ProjectItem::new(1, "Demo".into(), directory.into());
    project.spec_locked = true;
    state.projects.push(project);
    std::fs::write(
        std::path::Path::new(&directory).join("state.json"),
        serde_json::to_vec(&state)?,
    )?;
    Ok(())
}
