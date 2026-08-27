#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["inspect-closeout"])
    parser.add_argument("--repo", required=True)
    parser.add_argument("--scope", default="changed_files")
    args = parser.parse_args()

    payload = {
        "verdict": "GREEN",
        "status": "results_available",
        "total_problems": 0,
        "requested_path": args.repo,
        "resolved_project_path": "{workspace}/parent-repo",
        "route_base_path": "{workspace}/parent-repo",
        "scope": args.scope,
        "scope_file_count": 0,
        "profile": "Default",
        "expected_profile": "OdooProbe",
        "session_fresh": True,
        "indexing_complete": True,
        "capture_complete": True,
        "inspection_completed": True,
        "inspection_ids_registered": [],
        "expected_inspection_ids": ["OdooPyInspection", "OdooXmlInspection"],
        "proof_failures": [
            "resolved_project_path does not match requested_path",
            "scope_file_count is zero for changed_files",
            "expected Odoo inspection profile was not applied",
            "expected Odoo inspections are not registered",
        ],
    }
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
