# Risk (#552, SPEC §2.2 and F6): interruption while reopening a settled thread can drop its row.
# Structural regression probe with the two project files at the interrupted save boundary.
from pathlib import Path
from tempfile import TemporaryDirectory
import json


source = Path("crates/velocity-store/src/storage.rs").read_text(encoding="utf-8")
write = source.split("fn write_changed_project(", 1)[1].split("\n#[cfg(test)]", 1)[0]
assemble = source.split("fn reassemble_threads(", 1)[1].split("fn root_metadata_changed(", 1)[0]

archive_removal = "let _ignored = fs::remove_file(&settled_path);"
active_replace = "&project_active_path(directory, current.id),"
if (archive_removal in write and active_replace in write
        and write.index(archive_removal) < write.index(active_replace)
        and "active.into_iter().chain(settled)" in assemble
        and "if let Some(thread) = by_id.remove(id)" in assemble):
    with TemporaryDirectory() as directory:
        active = Path(directory) / "1.json"
        settled = Path(directory) / "1.settled.json"
        active.write_text(json.dumps({"project": {"threads": []}, "thread_order": [7]}), encoding="utf-8")
        settled.write_text(json.dumps({"threads": [{"id": 7, "state": "merged"}]}), encoding="utf-8")
        print("Thread 7 exists only in the settled file before its revert.")
        settled.unlink()
        print("The saver removes the settled file first. Interrupt before replacing 1.json.")
        active_file = json.loads(active.read_text(encoding="utf-8"))
        settled_rows = json.loads(settled.read_text(encoding="utf-8"))["threads"] if settled.exists() else []
        rows = {thread["id"]: thread for thread in active_file["project"]["threads"] + settled_rows}
        print(f"Thread order on restart: {active_file['thread_order']}; available rows: {list(rows)}.")
        if 7 not in rows:
            raise SystemExit("An interrupted reopen must preserve either the settled or active thread row.")
