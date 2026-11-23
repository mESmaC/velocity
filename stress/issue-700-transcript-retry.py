# Risk (#552, SPEC §2.2): retrying a partly written batch duplicates transcript blocks.
# Structural regression probe with a temporary JSONL file for the matched append path.
from pathlib import Path
from tempfile import TemporaryDirectory
import json


def source(name):
    return Path(f"crates/velocity-store/src/{name}.rs").read_text(encoding="utf-8")


save = source("save_behind")
storage = source("storage")
transcripts = source("transcripts")
take = save.split("fn take_save_batch(", 1)[1].split("fn restore_failed_batch(", 1)[0]
restore = save.split("fn restore_failed_batch(", 1)[1].split("fn finish_save_batch(", 1)[0]
pending = save.split("pub fn save_pending(", 1)[1].split("#[cfg(test)]", 1)[0]
write = transcripts.split("pub fn write_transcript(", 1)[1].split("fn serialize_jsonl(", 1)[0]
batch = storage.split("pub(crate) fn write_incremental_snapshot(", 1)[1].split("fn write_changed_project(", 1)[0]

if (".persisted_transcripts" in take
        and "guard.pending_transcripts.entry(key).or_insert(blocks)" in restore
        and "persisted_transcripts" not in restore
        and pending.index("restore_failed_batch(store, batch.pending, batch.deleted") < pending.index("finish_save_batch(")
        and "previous_transcripts.get(&(project_id, thread_id))" in batch
        and "for (&(project_id, thread_id), blocks) in pending_transcripts" in batch
        and "current[..prev.len()] == prev[..]" in write
        and "return append_blocks(&path, &current[prev.len()..]);" in write
        and "fs::read" not in write and "read_transcript" not in write):
    with TemporaryDirectory() as directory:
        path = Path(directory) / "1.jsonl"
        previous = [{"UserMessage": {"text": "prior block", "queued": False}}]
        current = previous + [{"AgentMessage": {"paragraphs": ["new block"]}}]
        path.write_text(json.dumps(previous[0]) + "\n", encoding="utf-8")
        for attempt in (1, 2):
            with path.open("a", encoding="utf-8") as transcript:
                for block in current[len(previous):]:
                    transcript.write(json.dumps(block) + "\n")
            if attempt == 1:
                print("Thread 1 appends successfully. A later transcript fails, so the entire batch is retried with the same confirmed prefix.")
        actual = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
        print(f"Expected {current!r}; recovered {actual!r}.")
        if actual != current:
            raise SystemExit("Retry must not append the same transcript suffix twice.")
