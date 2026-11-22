"""Check the retained-task reland path and its Goal Swarm eligibility."""
from pathlib import Path
import re
import sys

root = Path.cwd()

def body(path, function, next_marker="\n}"):
    contents = (root / path).read_text(encoding="utf-8")
    return contents.split("fn " + function + "(", 1)[1].split(next_marker, 1)[0]

def require(condition, detail):
    if not condition:
        raise RuntimeError(detail)

store = "crates/velocity-store/src/"
inventory = "crates/velocity-threads/src/work_pool.rs"
delete = body(store + "threads.rs", "remove_thread_record")
projection_delete = body(store + "rosen_work.rs", "remove_thread")
reopen = body(store + "rosen_work.rs", "reopen")
apply = body(store + "rosen_work.rs", "apply")
record = body(store + "rosen.rs", "record_rosen_progress")
pool = body(inventory, "task_pieces")
goal = body(store + "goal_swarms.rs", "bind_piece_owner").split("WorkKey::Issue", 1)[0]
dispatch = body(store + "continuous_dispatch.rs", "available_task")

require("crate::rosen_work::remove_thread(item, thread)" in delete,
        "Thread deletion no longer uses the work projection removal path")
require("task.assigned_thread_id = None" in delete and
        "if task.state == TaskState::Now" in delete,
        "Thread deletion no longer leaves existing Done tasks unowned")
require("item.thread_identifier = None" in projection_delete and
        "item.status.completed()" in projection_delete,
        "Thread deletion no longer preserves detached completion")
require("item.thread_identifier.is_none()" in reopen and
        "task.state == TaskState::Done" in reopen and
        "task.state = TaskState::Todo" in reopen,
        "Detached revert no longer reopens the legacy Task List entry")
detached = re.search(r"let Some\(index\) = thread else \{(.*?)\n\s*\};", record, re.S)
require(detached and "crate::rosen_work::apply" in detached[1] and
        "self.store_snapshot(next)?" in detached[1] and "return Ok(None)" in detached[1],
        "Detached events no longer use the early projection-only path")
require("WorkStatus::Landed" in apply and
        "RosenProgress::Landed if !item.status.completed() || replaced_land" in apply and
        "finish_thread_tasks" in record,
        "Could not verify the projected reland and legacy completion branches")
require("TaskState::Todo | TaskState::Now" in pool and
        "task.assigned_thread_id" in goal and "TaskState::Todo" in goal,
        "Could not verify the Goal Swarm availability and claim predicates")
require("item.status.completed()" in dispatch,
        "Could not verify that continuous dispatch handles projected completion")

if ("TaskState::Done" not in apply and "finish_thread_tasks" not in detached[1]
        and "rosen_work_item" not in pool and "rosen_work_item" not in goal
        and "available_task(" not in pool and "available_task(" not in goal):
    print("FAIL: land -> delete thread history -> revert -> reland leaves the "
          "persisted task Todo/unassigned with a Landed WorkItem. "
          "Continuous dispatch excludes it, but the Goal Swarm pool and its "
          "claim-time validation still accept it.", file=sys.stderr)
    sys.exit(1)
print("The detached reland or Goal Swarm eligibility path has changed.")
