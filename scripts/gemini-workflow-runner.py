#!/usr/bin/env python3
"""A real Gemini-backed harness runner for escurel dynamic workflows.

Speaks the ADK adapter's I/O contract (crates/escurel-runner-harness/src/adk.rs):
  - stdin:  AdkTask JSON { instructions, input, mcp_endpoint, allowed_tools }
  - env:    ESCUREL_MCP_BEARER (the scoped /mcp token), GEMINI_API_KEY,
            LLM_MODEL (optional; default gemini-2.0-flash)
  - stdout: HarnessOutcome JSON { ok, status, summary, tool_calls, produced_instance }

It is a genuine agent: it reads the phase step from the inbox, gathers the run's
context (the board + every prior phase's produced instances) over /mcp, asks
Gemini to author the target instance's body, and writes it back with update_page
+ assign_event. No mocks — every escurel effect is a real /mcp call and every
phase body is real Gemini output.
"""
import json
import os
import sys
import urllib.request

GEMINI_URL = "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}"


def mcp(endpoint, bearer, name, args):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
         "params": {"name": name, "arguments": args}}
    ).encode()
    req = urllib.request.Request(
        endpoint, data=body,
        headers={"content-type": "application/json", "authorization": f"Bearer {bearer}"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        resp = json.load(r)
    if "error" in resp:
        raise RuntimeError(f"mcp {name}: {resp['error']}")
    result = resp.get("result", {})
    return result.get("structuredContent", result)


def gemini(key, model, prompt):
    url = GEMINI_URL.format(model=model, key=key)
    body = json.dumps({"contents": [{"parts": [{"text": prompt}]}]}).encode()
    req = urllib.request.Request(url, data=body, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=90) as r:
        resp = json.load(r)
    return resp["candidates"][0]["content"]["parts"][0]["text"]


def strip_fences(text):
    t = text.strip()
    if t.startswith("```"):
        t = t.split("\n", 1)[-1] if "\n" in t else t
        if t.endswith("```"):
            t = t[: t.rfind("```")]
    return t.strip()


def run_slug(run_page):
    seg = run_page.rsplit("/", 1)[-1]
    return seg[:-3] if seg.endswith(".md") else seg


def done(summary, produced, calls):
    print(json.dumps({
        "ok": True, "status": "ok", "summary": summary,
        "tool_calls": calls, "produced_instance": produced,
    }))


def main():
    task = json.load(sys.stdin)
    endpoint = task["mcp_endpoint"]
    bearer = os.environ["ESCUREL_MCP_BEARER"]
    key = os.environ["GEMINI_API_KEY"]
    model = os.environ.get("LLM_MODEL") or "gemini-2.0-flash"
    calls = 0

    inbox = mcp(endpoint, bearer, "list_inbox", {}); calls += 1
    target = next(
        (e for e in reversed(inbox.get("events", [])) if e.get("instance_page_id")),
        None,
    )
    if target is None:
        return done("no inbox event with a target instance", None, calls)

    page = target["instance_page_id"]
    event_id = target["event_id"]
    wf = (target.get("provenance") or {}).get("workflow") or {}
    phase = wf.get("phase", "")
    run = wf.get("run", "")
    wf_skill = wf.get("wf_skill", "")

    rest = page.split("markdown/instances/", 1)[-1]
    skill, fname = rest.split("/", 1)
    inst_id = fname[:-3] if fname.endswith(".md") else fname

    # The invocation records the question into the run board verbatim (no LLM):
    # every downstream phase reads it from there.
    if skill == "workflow-run":
        question = target.get("body", "").strip() or "(no question provided)"
        content = (
            f"---\ntype: instance\nskill: workflow-run\nid: {inst_id}\n"
            f"wf_skill: {wf_skill}\nstatus: running\n---\n"
            f"# run {inst_id}\n\n## question\n{question}\n"
        )
        mcp(endpoint, bearer, "update_page", {"page_id": page, "content": content}); calls += 1
        mcp(endpoint, bearer, "assign_event",
            {"event_id": event_id, "instance_page_id": page}); calls += 1
        return done(f"recorded question into board {page}", page, calls)

    # Gather run context: the board + every prior phase's produced instances.
    context = []
    if run:
        try:
            board = mcp(endpoint, bearer, "expand", {"page_id": run}); calls += 1
            context.append("RUN BOARD:\n" + (board.get("body") or ""))
        except Exception:
            pass
    if wf_skill:
        try:
            plan = mcp(endpoint, bearer, "expand",
                       {"page_id": f"markdown/skills/{wf_skill}.md"}); calls += 1
            phases = plan.get("frontmatter", {}).get("phases", []) or []
            slug = run_slug(run)
            seen = set()
            for ph in phases:
                if ph.get("id") == phase:
                    break  # only prior phases
                produces = ph.get("produces")
                if not produces or produces in seen:
                    continue
                seen.add(produces)
                listed = mcp(endpoint, bearer, "list_instances",
                             {"skill_id": produces}); calls += 1
                prefix = f"markdown/instances/{produces}/{slug}-"
                for inst in listed.get("instances", []):
                    if inst.get("page_id", "").startswith(prefix):
                        ex = mcp(endpoint, bearer, "expand",
                                 {"page_id": inst["page_id"]}); calls += 1
                        context.append(f"{produces.upper()} INSTANCE:\n" + (ex.get("body") or ""))
        except Exception:
            pass

    prompt = (
        f"You are a research workflow agent executing phase '{phase}'. "
        f"Follow these phase instructions:\n\n{task['instructions']}\n\n"
        f"Your task input:\n{task['input']}\n\n"
        f"Context gathered from the run so far:\n" + "\n\n".join(context) + "\n\n"
        f"Write ONLY the markdown BODY for the '{skill}' instance (no YAML frontmatter, "
        f"no code fences). Be substantive but concise."
    )
    body_text = strip_fences(gemini(key, model, prompt))
    content = f"---\ntype: instance\nskill: {skill}\nid: {inst_id}\n---\n{body_text}\n"
    mcp(endpoint, bearer, "update_page", {"page_id": page, "content": content}); calls += 1
    mcp(endpoint, bearer, "assign_event",
        {"event_id": event_id, "instance_page_id": page}); calls += 1
    return done(f"gemini authored {page} ({len(body_text)} chars)", page, calls)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # adapter-level failure → non-zero exit + stderr
        sys.stderr.write(f"gemini-workflow-runner: {exc}\n")
        sys.exit(1)
