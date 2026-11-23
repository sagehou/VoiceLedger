import json
from pathlib import Path

from jsonschema import Draft202012Validator


contracts = Path(__file__).resolve().parents[1] / "contracts"
schema = json.loads((contracts / "test-evidence-v1.schema.json").read_text(encoding="utf-8"))
fixture = json.loads((contracts / "fixtures/test-evidence-v1.json").read_text(encoding="utf-8"))

Draft202012Validator.check_schema(schema)
validator = Draft202012Validator(schema)
validator.validate(fixture)
if validator.is_valid({**fixture, "status": "pass"}):
    raise SystemExit("Evidence marked pass must include the observed outcome and evidence path")
print("Test evidence schema and synthetic fixture validated")
