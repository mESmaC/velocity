# Risk (#702, #552, SPEC §2.2 and §3.2): a truncated UTF-8 append can block store startup.
# Structural regression probe with a real byte-truncated JSONL file.
# Adapted from scout-552-transcript-crash.py after #700 moved load_split_project from
# transcripts::read_transcript(...) to transcripts::read_transcript_persisted(...), preserving
# the exact check that load_split_project propagates a whole-file read_to_string error.
from pathlib import Path
from tempfile import TemporaryDirectory
import json


store = Path("crates/velocity-store/src/storage.rs").read_text(encoding="utf-8")
transcripts = Path("crates/velocity-store/src/transcripts.rs").read_text(encoding="utf-8")
load = store.split("fn load_split_project(", 1)[1].split("fn reassemble_threads(", 1)[0]
read = transcripts.split("pub fn read_transcript(", 1)[1].split("fn recover_partial_transcript(", 1)[0]

if (("transcripts::read_transcript(directory, project_id, thread.id)?" in load
        or "transcripts::read_transcript_persisted(directory, project_id, thread.id)?" in load)
        and "match fs::read_to_string(&path)" in read
        and read.index("return Err(StoreError::ReadSnapshot") < read.index("parse_jsonl_blocks(")):
    with TemporaryDirectory() as directory:
        path = Path(directory) / "7.jsonl"
        complete = json.dumps({"AgentMessage": {"paragraphs": ["previously saved"]}}).encode("utf-8") + b"\n"
        interrupted = b'{"AgentMessage":{"paragraphs":["' + "é".encode("utf-8")[:1]
        path.write_bytes(complete + interrupted)
        assert json.loads(path.read_bytes().splitlines()[0])["AgentMessage"]["paragraphs"] == ["previously saved"]
        try:
            path.read_text(encoding="utf-8")
        except UnicodeDecodeError as error:
            print(f"UTF-8 read fails at byte {error.start} before line parsing; the complete first line remains readable.")
            raise SystemExit("One torn append must not make an active project's saved state unloadable.") from None
