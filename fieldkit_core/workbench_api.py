"""Validated operations shared by the browser API and fixed-case MCP tools."""
from __future__ import annotations

from .common import Failure
from .manifest import COMMANDS, validate


def text(maximum=20000, minimum=0):
    return {"type": "string", "minLength": minimum, "maxLength": maximum}


IDS = {"type": "array", "items": text(256, 1), "maxItems": 50}
JOB = {"command": {"type": "string", "enum": list(COMMANDS)}, "target": text(8192, 1),
       "options": {"type": "object"}}
OPERATIONS = {
    "case": {"description": "Set the investigation title and research question", "required": ["title"],
             "properties": {"title": text(160, 1), "question": text(2000)}},
    "evidence": {"description": "Capture an analyst-provided evidence note with source context", "required": ["title"],
                 "properties": {"title": text(240, 1), "content": text(), "source_url": text(8192), "kind": text(40, 1), "observed_at": text(80)}},
    "evidence/review": {"description": "Record an analyst evidence assessment and rationale", "required": ["evidence_id", "status"],
                        "properties": {"evidence_id": text(256, 1), "status": {"enum": ["unreviewed", "corroborated", "contested", "discarded"]}, "note": text(4000)}},
    "notes": {"description": "Add a case note linked to supporting evidence", "required": ["content"],
              "properties": {"content": text(20000, 1), "evidence_ids": IDS}},
    "entities": {"description": "Add an explicitly labeled case entity", "required": ["kind", "value"],
                 "properties": {"kind": text(40, 1), "value": text(2048, 1), "label": text(160)}},
    "links": {"description": "Record an analyst relationship supported by evidence references",
              "required": ["source_id", "target_id", "relation", "evidence_ids"],
              "properties": {"source_id": text(256, 1), "target_id": text(256, 1), "relation": text(80, 1), "evidence_ids": {**IDS, "minItems": 1}, "note": text(4000)}},
    "flows": {"description": "Create a bounded investigation flow", "required": ["title"],
              "properties": {"title": text(160, 1), "steps": {"type": "array", "maxItems": 20,
                  "items": {"type": "object", "additionalProperties": False, "required": ["command"], "properties": JOB}}}},
    "flows/steps": {"description": "Add a validated collector step to a flow", "required": ["flow_id", "command"],
                    "properties": {"flow_id": text(256, 1), **JOB}},
    "flows/run": {"description": "Run one investigation step and save its evidence in this case", "required": ["flow_id", "step_id"],
                  "properties": {"flow_id": text(256, 1), "step_id": text(256, 1)}},
    "tools/run": {"description": "Run a collector and save the report and sources to this case", "required": ["command"], "properties": JOB},
    "correlations/generate": {"description": "Generate reviewable leads with supporting evidence; identities stay separate", "properties": {}},
    "correlations/review": {"description": "Accept or reject a correlation lead with analyst rationale", "required": ["id", "decision", "note"],
                            "properties": {"id": text(256, 1), "decision": {"enum": ["accepted", "rejected"]}, "note": text(4000, 1)}},
    "demo": {"description": "Seed an empty case with explicitly synthetic training evidence", "properties": {}},
}


def operation_schema(name):
    descriptor = OPERATIONS[name]
    return {"type": "object", "additionalProperties": False, "properties": descriptor["properties"],
            "required": descriptor.get("required", [])}


def apply_operation(model, name, payload):
    if name not in OPERATIONS:
        raise Failure("WORKBENCH_OPERATION", "Unknown case workspace operation", "invalid")
    validate(payload, operation_schema(name), "arguments")
    if name == "case":
        return model.update_case(payload["title"], payload.get("question", ""))
    if name == "evidence":
        return model.add_evidence(**payload)
    if name == "evidence/review":
        return model.review_evidence(payload["evidence_id"], payload["status"], payload.get("note", ""))
    if name == "notes":
        return model.add_note(payload["content"], payload.get("evidence_ids"))
    if name == "entities":
        return model.add_entity(**payload)
    if name == "links":
        return model.add_link(**payload)
    if name == "flows":
        return model.create_flow(payload["title"], payload.get("steps"))
    if name == "flows/steps":
        return model.add_step(payload["flow_id"], payload["command"], payload.get("target"), payload.get("options"))
    if name == "flows/run":
        return model.run_step(payload["flow_id"], payload["step_id"])
    if name == "tools/run":
        return model.run_tool(payload["command"], payload.get("target"), payload.get("options"))
    if name == "correlations/generate":
        return model.correlations()
    if name == "correlations/review":
        return model.review_correlation(payload["id"], payload["decision"], payload["note"])
    return model.seed_demo()
