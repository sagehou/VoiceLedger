import copy
import hashlib
import json
import re
import sys
from pathlib import Path

from jsonschema import Draft202012Validator


def load_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def apply_change(document, change):
    pointer = change["path"]
    if not pointer.startswith("/"):
        raise ValueError("Each fixture change must name a JSON pointer")
    keys = [key.replace("~1", "/").replace("~0", "~") for key in pointer[1:].split("/")]
    parent = document
    for key in keys[:-1]:
        parent = parent[int(key) if isinstance(parent, list) else key]
    key = keys[-1]
    parent[int(key) if isinstance(parent, list) else key] = copy.deepcopy(change["value"])


def main(output):
    contracts = Path(__file__).resolve().parents[1] / "contracts"
    schema = load_json(contracts / "domain-v1.schema.json")
    baseline = load_json(contracts / "fixtures/domain-v1.json")
    cases = load_json(contracts / "fixtures/domain-v1-cases.json")
    Draft202012Validator.check_schema(schema)
    validator = Draft202012Validator(schema)
    validator.validate(baseline)

    output.mkdir(parents=True, exist_ok=True)
    names = set()
    rows = []
    accepted_shapes = 0
    for case in cases:
        name = case["name"]
        if not re.fullmatch(r"[a-z][a-z0-9_]+", name) or name in names:
            raise ValueError("Fixture names must be unique lowercase identifiers")
        names.add(name)
        document = copy.deepcopy(baseline)
        for change in case["changes"]:
            apply_change(document, change)

        errors = list(validator.iter_errors(document))
        valid_shape = not errors
        if valid_shape != case["schema_valid"]:
            violations = [f"{list(error.absolute_path)} ({error.validator})" for error in errors]
            raise ValueError(f"Unexpected schema result for {name}: {violations}")
        accepted_shapes += int(valid_shape)
        payload = (json.dumps(document, ensure_ascii=False, allow_nan=False, separators=(",", ":")) + "\n").encode()
        (output / f"{name}.json").write_bytes(payload)
        rows.append(f"{name}\t{case['expected_status']}\t{hashlib.sha256(payload).hexdigest()}\n")

    if not 0 < accepted_shapes < len(rows):
        raise ValueError("The domain suite must exercise both valid and invalid JSON shapes")
    index = "".join(rows).encode()
    (output / "cases.tsv").write_bytes(index)
    print(f"Domain schema v1 checked {len(rows)} cases ({accepted_shapes} valid shapes).")
    print(f"Domain suite SHA-256 {hashlib.sha256(index).hexdigest()}")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
