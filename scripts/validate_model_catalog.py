import json
from pathlib import Path

from jsonschema import Draft202012Validator


def main():
    root = Path(__file__).resolve().parents[1]
    schema = json.loads((root / "contracts/model-catalog-v1.schema.json").read_text(encoding="utf-8"))
    catalog = json.loads((root / "models/catalog-v1.json").read_text(encoding="utf-8"))
    Draft202012Validator.check_schema(schema)
    Draft202012Validator(schema).validate(catalog)
    print(f"Model catalog v1 validated {len(catalog['models'])} candidate records.")


if __name__ == "__main__":
    main()
