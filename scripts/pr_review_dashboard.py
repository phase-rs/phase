#!/usr/bin/env python3
"""Render the PR-review JSON snapshot as a self-contained, auto-refreshing page."""

from __future__ import annotations

import argparse
import html
import json
import os
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


AUTO_REFRESH_SECONDS = 60


def text(value: Any, fallback: str = "—") -> str:
    if value is None or value == "":
        return fallback
    return html.escape(str(value), quote=True)


def label(value: Any, fallback: str = "Unknown") -> str:
    if value is None or value == "":
        return fallback
    return html.escape(str(value).replace("_", " ").replace("-", " ").title())


def attribute(value: Any) -> str:
    return html.escape(str(value or ""), quote=True)


def search_text(row: dict[str, Any]) -> str:
    return attribute(
        " ".join(
            str(row.get(key) or "")
            for key in ("pr", "title", "author_login", "advisory_action", "reason", "ci", "state")
        ).casefold()
    )


def parse_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed if parsed.tzinfo is not None else parsed.replace(tzinfo=UTC)


def time_element(value: Any, reference: Any = None) -> str:
    exact = text(value)
    moment = parse_timestamp(value)
    baseline = parse_timestamp(reference)
    if moment is None:
        return f"<time>{exact}</time>"
    display = moment.astimezone(UTC).strftime("%b %-d, %Y %H:%M UTC")
    if baseline is not None:
        seconds = int((baseline - moment).total_seconds())
        future = seconds < 0
        elapsed = abs(seconds)
        if elapsed < 60:
            display = "just now"
        elif elapsed < 60 * 60:
            display = f"{elapsed // 60}m"
        elif elapsed < 24 * 60 * 60:
            display = f"{elapsed // (60 * 60)}h"
        elif elapsed < 14 * 24 * 60 * 60:
            display = f"{elapsed // (24 * 60 * 60)}d"
        else:
            display = f"{elapsed // (7 * 24 * 60 * 60)}w"
        display = f"in {display}" if future else f"{display} ago"
    return f'<time datetime="{exact}" title="{exact}">{html.escape(display)}</time>'


def event_text(event: Any, fallback: str, reference: Any = None) -> str:
    if not isinstance(event, dict):
        return fallback
    timestamp = time_element(event.get("timestamp"), reference)
    event_label = label(event.get("event_type"), "Recorded")
    outcome = event.get("outcome")
    summary = text(event.get("summary"), "")
    parts = [timestamp, event_label]
    if outcome:
        parts.append(label(outcome))
    if summary:
        parts.append(summary)
    if event.get("head_matches_current") is False:
        parts.append('<span class="stale-head">previous head</span>')
    return " · ".join(parts)


def tone_for(action: Any) -> str:
    if action in {"hard_stop", "request_changes", "blocked"}:
        return "danger"
    if action in {"hold", "hold_ci", "warn_stale_changes_for_handler"}:
        return "warning"
    if action in {"review", "approve_ready_for_handler", "queued"}:
        return "ready"
    return "quiet"


def title_link(row: dict[str, Any], checked_at: Any) -> str:
    number = text(row.get("pr"), "?")
    title = text(row.get("title"), "Untitled pull request")
    url = row.get("url")
    author = text(row.get("author_login"), "unknown author")
    updated = time_element(row.get("updated_at"), checked_at)
    content = (
        f'<span class="pr-title">#{number} {title}</span>'
        f'<span class="pr-meta">@{author} · updated {updated}</span>'
    )
    if not isinstance(url, str) or not url:
        return f'<span class="pr-link unavailable">{content}</span>'
    return (
        f'<a class="pr-link" href="{html.escape(url, quote=True)}" '
        f'target="_blank" rel="noopener noreferrer">{content}'
        '<span class="sr-only"> (opens on GitHub in a new tab)</span></a>'
    )


def detail_row(name: str, value: str) -> str:
    return f"<div><dt>{html.escape(name)}</dt><dd>{value}</dd></div>"


def history_rows(row: dict[str, Any], checked_at: Any) -> str:
    history = row.get("local_history") or {}
    return "".join(
        (
            detail_row("GitHub checked", time_element(checked_at)),
            detail_row(
                "Last recorded look",
                event_text(
                    history.get("last_recorded_look"),
                    "No review activity recorded",
                    checked_at,
                ),
            ),
            detail_row(
                "Last material action",
                event_text(
                    history.get("last_material_action"),
                    "No material action recorded",
                    checked_at,
                ),
            ),
        )
    )


def compact_event(event: Any, fallback: str, checked_at: Any) -> str:
    if not isinstance(event, dict):
        return f'<span class="muted">{html.escape(fallback)}</span>'
    summary = text(event.get("summary"), "No summary")
    stale_head = (
        '<span class="stale-head">previous head</span>'
        if event.get("head_matches_current") is False
        else ""
    )
    return (
        f'<span class="event-kind">{label(event.get("event_type"), "Recorded")}</span>'
        f'<span class="event-summary">{summary}</span>'
        f'<span class="event-time">{time_element(event.get("timestamp"), checked_at)}</span>{stale_head}'
    )


def compact_look(event: Any, checked_at: Any) -> str:
    if not isinstance(event, dict):
        return '<span class="muted">Never recorded</span>'
    stale_head = (
        '<span class="stale-head">previous head</span>'
        if event.get("head_matches_current") is False
        else ""
    )
    return (
        f'<span class="look-time">{time_element(event.get("timestamp"), checked_at)}</span>'
        f'<span class="look-source">via {label(event.get("event_type"), "Recorded activity")}</span>'
        f"{stale_head}"
    )


def ci_status(value: Any) -> str:
    state = str(value or "unknown").lower()
    tone, symbol, accessible_label = {
        "green": ("ci-success", "✓", "CI passing"),
        "pending": ("warning", "●", "CI pending"),
        "failed": ("danger", "×", "CI failing"),
        "unknown": ("quiet", "?", "CI status unknown"),
    }.get(state, ("quiet", "?", "CI status unknown"))
    return (
        f'<span class="ci-status {tone}" role="img" '
        f'aria-label="{accessible_label}" title="{accessible_label}">{symbol}</span>'
    )


def candidate_evidence(row: dict[str, Any]) -> str:
    hard_stops = row.get("hard_stop_paths") or []
    artifacts = row.get("artifacts") or {}
    artifact_failures = artifacts.get("failures") or []
    freshness = row.get("freshness") or {}
    evidence = []
    if hard_stops:
        evidence.append("Hard-stop paths: " + ", ".join(text(path) for path in hard_stops))
    if artifact_failures:
        evidence.append("Artifact failures: " + ", ".join(text(item) for item in artifact_failures))
    if (row.get("proof") or {}).get("proof_gap"):
        evidence.append("Proof required before a ready recommendation")
    if freshness.get("head_changed_since_local_event"):
        evidence.append("Current head differs from the latest local record")
    if freshness.get("author_followup_after_maintainer_activity"):
        evidence.append("Author activity followed maintainer activity")
    if not evidence:
        return "<p class=\"muted\">No additional dashboard evidence.</p>"
    return "<ul>" + "".join(f"<li>{item}</li>" for item in evidence) + "</ul>"


def render_candidate(row: dict[str, Any], checked_at: Any) -> str:
    action = row.get("advisory_action")
    history = row.get("local_history") or {}
    details = "".join(
        (
            detail_row("Surface", label(row.get("surface"))),
            detail_row("Gate", label(row.get("gate"))),
            detail_row("Review decision", label(row.get("review_decision"), "None")),
            detail_row("Last GitHub update", time_element(row.get("updated_at"), checked_at)),
            detail_row("Current head", f"<code>{text(row.get('head_sha'))}</code>"),
            detail_row(
                "Last recorded activity",
                event_text(history.get("last_recorded_activity"), "No local record", checked_at),
            ),
        )
    )
    reason = label(row.get("reason"), "No reason supplied")
    pr_number = int(row.get("pr") or 0)
    detail_id = f"open-details-{pr_number}"
    return f"""
      <tr class="queue-row" data-kind="open" data-section="open" data-status="{attribute(action)}" data-ci="{attribute(str(row.get('ci') or '').lower())}" data-search="{search_text(row)}">
        <td class="pr-cell">{title_link(row, checked_at)}</td>
        <td><span class="status-label {tone_for(action)}">{label(action)}</span><span class="reason">{reason}</span></td>
        <td class="ci-cell">{ci_status(row.get("ci"))}</td>
        <td>{compact_event(history.get("last_material_action"), "No material action", checked_at)}</td>
        <td>{compact_look(history.get("last_recorded_look"), checked_at)}</td>
        <td class="details-cell"><button type="button" class="details-button" data-detail-target="{detail_id}" aria-expanded="false">Details</button></td>
      </tr>
      <tr id="{detail_id}" class="detail-row" hidden>
        <td colspan="6"><div class="detail-panel"><dl class="facts">{history_rows(row, checked_at)}{details}</dl>{candidate_evidence(row)}</div></td>
      </tr>
    """


def render_terminal_row(row: dict[str, Any], checked_at: Any) -> str:
    state = row.get("state")
    terminal_at = row.get("merged_at") or row.get("closed_at") or row.get("updated_at")
    state_label = "Closed without merge" if state == "CLOSED" else "Merged"
    history = row.get("local_history") or {}
    pr_number = int(row.get("pr") or 0)
    detail_id = f"terminal-details-{pr_number}"
    section = attribute(row.get("dashboard_section"))
    terminal_status = str(state or "").lower()
    return f"""
      <tr class="queue-row terminal" data-kind="terminal" data-section="{section}" data-status="{attribute(terminal_status)}" data-ci="" data-search="{search_text(row)}">
        <td class="pr-cell">{title_link(row, checked_at)}</td>
        <td><span class="status-label {tone_for('quiet')}">{state_label}</span></td>
        <td>{time_element(terminal_at, checked_at)}</td>
        <td>{compact_event(history.get("last_material_action"), "No material action", checked_at)}</td>
        <td>{compact_look(history.get("last_recorded_look"), checked_at)}</td>
        <td class="details-cell"><button type="button" class="details-button" data-detail-target="{detail_id}" aria-expanded="false">Details</button></td>
      </tr>
      <tr id="{detail_id}" class="detail-row" hidden>
        <td colspan="6"><div class="detail-panel"><dl class="facts">{detail_row("Author", text(row.get("author_login")))}{history_rows(row, checked_at)}</dl></div></td>
      </tr>
    """


def render_table(
    rows: Any,
    renderer: Any,
    checked_at: Any,
    terminal: bool = False,
    section: str = "open",
) -> str:
    if not isinstance(rows, list) or not rows:
        return '<p class="empty">None.</p>'
    third_heading = "Terminal at" if terminal else "CI"
    caption = "Terminal pull requests" if terminal else "Open pull requests"
    rendered_rows = "".join(
        renderer({**row, "dashboard_section": section}, checked_at)
        for row in rows
        if isinstance(row, dict)
    )
    return f"""
      <div class="table-wrap">
        <table>
          <caption class="sr-only">{caption}</caption>
          <thead><tr><th scope="col">Pull request</th><th scope="col">Status</th><th scope="col">{third_heading}</th><th scope="col">Last material action</th><th scope="col">Last recorded look</th><th scope="col"><span class="sr-only">Details</span></th></tr></thead>
          <tbody>{rendered_rows}</tbody>
        </table>
      </div>
    """


def render_dashboard(snapshot: dict[str, Any]) -> str:
    generated_at = snapshot.get("generated_at")
    candidates_by_action = snapshot.get("candidates_by_action") or {}
    candidates = [
        row
        for rows in candidates_by_action.values()
        if isinstance(rows, list)
        for row in rows
        if isinstance(row, dict)
    ]
    dashboard = snapshot.get("dashboard") or {}
    closed = dashboard.get("closed_unmerged") or {}
    closed_recent = closed.get("recent") or []
    closed_archive = closed.get("archive") or []
    merged = dashboard.get("merged") or []
    action_counts = snapshot.get("action_counts") or {}
    total_prs = len(candidates) + len(closed_recent) + len(closed_archive) + len(merged)
    status_summary = (
        f'<button type="button" class="status-count quiet" data-status-filter="" aria-pressed="true"><strong>{total_prs}</strong> All</button>'
        + "".join(
            f'<button type="button" class="status-count {tone_for(action)}" data-status-filter="{attribute(action)}" aria-pressed="false"><strong>{text(count, "0")}</strong> {label(action)}</button>'
            for action, count in action_counts.items()
        )
        + (
            f'<button type="button" class="status-count quiet" data-status-filter="closed" aria-pressed="false"><strong>{len(closed_recent) + len(closed_archive)}</strong> Closed</button>'
            if closed_recent or closed_archive
            else ""
        )
        + (
            f'<button type="button" class="status-count ready" data-status-filter="merged" aria-pressed="false"><strong>{len(merged)}</strong> Merged</button>'
            if merged
            else ""
        )
    )
    ci_states = {str(row.get("ci") or "unknown").lower() for row in candidates}
    ci_order = [state for state in ("failed", "pending", "green", "unknown") if state in ci_states]
    ci_order.extend(sorted(ci_states - set(ci_order)))
    ci_options = "".join(
        f'<option value="{attribute(state)}">{label(state)}</option>'
        for state in ci_order
    )
    warnings = snapshot.get("warnings") or []
    warning_html = "".join(f"<li>{text(warning)}</li>" for warning in warnings)
    terminal_sync = dashboard.get("terminal_sync") or {}
    archive_summary = (
        "Terminal history is retained in this derived snapshot."
        if not terminal_sync.get("truncated")
        else "The latest terminal query reached its configured limit; older history remains from prior snapshots."
    )
    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta http-equiv="refresh" content="{AUTO_REFRESH_SECONDS}">
  <title>PR review dashboard</title>
  <style>
    :root {{ color-scheme: light dark; --page:#f2f2f7; --surface:#fff; --ink:#111; --muted:rgba(60,60,67,.7); --line:rgba(60,60,67,.29); --accent:#0a5ac2; --success:#248a3d; --danger:#c5221f; --warning:#9b5700; --quiet:#555860; }}
    @media (prefers-color-scheme: dark) {{ :root {{ --page:#000; --surface:#1c1c1e; --ink:#fff; --muted:rgba(235,235,245,.6); --line:rgba(84,84,88,.65); --accent:#72aaff; --success:#30d158; --danger:#ff9b93; --warning:#ffd08a; --quiet:#c5c5ca; }} }}
    * {{ box-sizing:border-box; }}
    body {{ margin:0; background:var(--page); color:var(--ink); font:15px/20px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }}
    main {{ max-width:1280px; margin:0 auto; padding:24px 16px 48px; }}
    h1,h2,h3,p {{ margin-top:0; }}
    h1 {{ margin-bottom:4px; font-size:28px; line-height:34px; letter-spacing:.36px; font-weight:400; }}
    h2 {{ margin-bottom:8px; font-size:20px; line-height:25px; letter-spacing:-.45px; font-weight:400; }}
    a {{ color:var(--accent); }}
    .subtitle,.muted,.empty {{ color:var(--muted); }}
    [hidden] {{ display:none !important; }}
    .status-summary {{ display:flex; flex-wrap:wrap; gap:8px; margin:18px 0 12px; }}
    .status-count {{ min-height:44px; padding:6px 10px; border:1px solid currentColor; border-radius:999px; background:transparent; font:inherit; font-size:13px; cursor:pointer; touch-action:manipulation; }} .status-count strong {{ font-weight:600; }} .status-count:hover {{ background:color-mix(in srgb, currentColor 8%, transparent); }} .status-count:active {{ opacity:.72; }} .status-count[aria-pressed="true"] {{ background:color-mix(in srgb, currentColor 14%, transparent); box-shadow:inset 0 0 0 1px currentColor; }} .status-count:focus-visible {{ outline:3px solid color-mix(in srgb, var(--accent) 35%, transparent); outline-offset:2px; }}
    .danger {{ color:var(--danger); }} .warning {{ color:var(--warning); }} .ready {{ color:var(--accent); }} .quiet {{ color:var(--quiet); }} .ci-success {{ color:var(--success); }}
    .filter-panel {{ display:flex; align-items:end; flex-wrap:wrap; gap:10px; margin-bottom:18px; }} .filter-field {{ display:flex; min-width:170px; flex-direction:column; gap:4px; color:var(--muted); font-size:12px; }} .filter-field.search {{ flex:1 1 320px; }} .filter-field input,.filter-field select {{ min-height:44px; border:1px solid var(--line); border-radius:8px; background:var(--surface); color:var(--ink); font:inherit; font-size:15px; line-height:20px; padding:9px 10px; }} .filter-field input:focus-visible,.filter-field select:focus-visible {{ outline:3px solid color-mix(in srgb, var(--accent) 35%, transparent); border-color:var(--accent); }} .clear-filters {{ min-height:44px; border:0; background:transparent; color:var(--accent); font:inherit; padding:8px 10px; cursor:pointer; }} .clear-filters:active {{ opacity:.72; }} .filter-result {{ flex:1 0 100%; color:var(--muted); font-size:13px; }}
    .notice {{ margin:0 0 18px; color:var(--warning); }} .notice ul {{ margin:0; padding-left:20px; }}
    section {{ margin-top:28px; }} .section-note {{ color:var(--muted); margin:-4px 0 10px; }}
    .table-wrap {{ background:var(--surface); border-radius:10px; overflow:hidden; }}
    table {{ width:100%; border-collapse:collapse; table-layout:fixed; }} th,td {{ padding:9px 12px; text-align:left; vertical-align:top; border-bottom:1px solid var(--line); }}
    th {{ position:sticky; top:0; background:var(--surface); color:var(--muted); font-size:12px; font-weight:400; letter-spacing:.08px; z-index:1; }}
    th:nth-child(1) {{ width:27%; }} th:nth-child(2) {{ width:15%; }} th:nth-child(3) {{ width:9%; }} th:nth-child(4),th:nth-child(5) {{ width:21%; }} th:nth-child(6) {{ width:7%; }}
    .queue-row:hover {{ background:color-mix(in srgb, var(--accent) 8%, transparent); }} .queue-row:last-of-type td {{ border-bottom:0; }}
    .pr-link {{ display:flex; min-height:44px; flex-direction:column; justify-content:center; color:var(--accent); text-decoration:none; border-radius:6px; }} .pr-link:hover .pr-title {{ text-decoration:underline; }} .pr-link:active {{ opacity:.72; }} .pr-link:focus-visible {{ outline:3px solid color-mix(in srgb, var(--accent) 35%, transparent); }} .pr-link.unavailable {{ color:var(--ink); }}
    .pr-title {{ display:block; font-size:15px; font-weight:600; line-height:20px; }} .pr-meta,.event-time {{ display:block; color:var(--muted); font-size:12px; line-height:16px; margin-top:2px; }}
    .status-label {{ display:block; font-size:12px; line-height:16px; font-weight:600; }}
    .ci-status {{ display:inline-flex; width:24px; height:24px; align-items:center; justify-content:center; font-size:20px; line-height:1; font-weight:700; }} .ci-status.warning {{ font-size:13px; }}
    .reason {{ display:block; margin-top:3px; color:var(--muted); font-size:12px; line-height:16px; }}
    .event-kind,.look-time {{ display:block; font-size:13px; line-height:16px; }} .event-summary {{ display:block; color:var(--muted); font-size:12px; line-height:16px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }} .look-source {{ display:block; color:var(--muted); font-size:12px; line-height:16px; margin-top:2px; }}
    .stale-head {{ display:inline-block; color:var(--warning); font-size:11px; line-height:13px; font-weight:600; }}
    .details-cell {{ text-align:right; }} .details-button {{ min-height:44px; border:0; background:transparent; color:var(--accent); font:inherit; padding:4px; cursor:pointer; touch-action:manipulation; }} .details-button:active {{ opacity:.72; }} .details-button:focus-visible {{ outline:3px solid color-mix(in srgb, var(--accent) 35%, transparent); border-radius:6px; }}
    .detail-row td {{ padding:0; border-bottom:1px solid var(--line); }} .detail-panel {{ padding:14px 16px 16px; background:color-mix(in srgb, var(--surface) 88%, var(--page)); }}
    .facts {{ display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:12px 18px; margin:0; }} .facts div {{ min-width:0; }} .facts dt {{ color:var(--muted); font-size:12px; line-height:16px; }} .facts dd {{ margin:2px 0 0; overflow-wrap:anywhere; }}
    .detail-panel ul {{ margin:14px 0 0; padding-left:20px; }} .archive {{ margin-top:18px; }} .archive summary {{ cursor:pointer; color:var(--accent); font-size:15px; }}
    .sr-only {{ position:absolute; width:1px; height:1px; padding:0; margin:-1px; overflow:hidden; clip:rect(0,0,0,0); white-space:nowrap; border:0; }}
    @media (max-width:850px) {{ th:nth-child(5),td:nth-child(5) {{ display:none; }} th:nth-child(1) {{ width:31%; }} th:nth-child(2) {{ width:20%; }} th:nth-child(3) {{ width:10%; }} th:nth-child(4) {{ width:30%; }} th:nth-child(6) {{ width:9%; }} }}
    @media (max-width:620px) {{ main {{ padding:16px 10px 36px; }} h1 {{ font-size:22px; line-height:28px; }} th,td {{ padding:8px; }} th:nth-child(3),td:nth-child(3) {{ display:none; }} th:nth-child(1) {{ width:42%; }} th:nth-child(2) {{ width:24%; }} th:nth-child(4) {{ width:25%; }} th:nth-child(6) {{ width:9%; }} .facts {{ grid-template-columns:1fr; }} .filter-field {{ flex:1 1 100%; }} }}
  </style>
</head>
<body>
  <main id="pr-review-dashboard">
    <header>
      <h1>Pull request review</h1>
      <p class="subtitle">All open PRs checked against GitHub {time_element(generated_at)}. This page reloads every {AUTO_REFRESH_SECONDS} seconds after the review job writes a newer snapshot.</p>
    </header>
    <section class="status-summary" aria-label="Filter by PR status">{status_summary}</section>
    <section class="filter-panel" aria-label="Dashboard filters">
      <label class="filter-field search" for="pr-search"><span>Search</span><input id="pr-search" type="search" placeholder="PR number, title, or author" autocomplete="off"></label>
      <label class="filter-field" for="ci-filter"><span>CI status</span><select id="ci-filter"><option value="">All CI states</option>{ci_options}</select></label>
      <button id="clear-filters" class="clear-filters" type="button">Clear filters</button>
      <output id="filter-result" class="filter-result">Showing {total_prs} PRs</output>
    </section>
    {f'<aside class="notice"><ul>{warning_html}</ul></aside>' if warning_html else ''}
    <section data-filter-section="open" aria-labelledby="open-heading">
      <h2 id="open-heading">Open queue (<span data-section-count="open">{len(candidates)}</span>)</h2>
      {render_table(candidates, render_candidate, generated_at, section="open")}
    </section>
    <section data-filter-section="recent_closed" aria-labelledby="recent-closed-heading">
      <h2 id="recent-closed-heading">Closed without merge — last 48 hours (<span data-section-count="recent_closed">{len(closed_recent)}</span>)</h2>
      <p class="section-note">Recent closures remain in the main review view for follow-up.</p>
      {render_table(closed_recent, render_terminal_row, generated_at, terminal=True, section="recent_closed")}
    </section>
    <section class="archive" data-filter-section="closed_archive" aria-labelledby="closed-archive-heading">
      <details>
        <summary id="closed-archive-heading">Closed without merge archive (<span data-section-count="closed_archive">{len(closed_archive)}</span>)</summary>
        <p class="section-note">{archive_summary}</p>
        {render_table(closed_archive, render_terminal_row, generated_at, terminal=True, section="closed_archive")}
      </details>
    </section>
    <section class="archive" data-filter-section="merged" aria-labelledby="merged-heading">
      <details>
        <summary id="merged-heading">Merged archive (<span data-section-count="merged">{len(merged)}</span>)</summary>
        {render_table(merged, render_terminal_row, generated_at, terminal=True, section="merged")}
      </details>
    </section>
  </main>
  <script>
    const dashboard = document.getElementById("pr-review-dashboard");
    const rows = Array.from(dashboard.querySelectorAll(".queue-row"));
    const statusButtons = Array.from(dashboard.querySelectorAll("[data-status-filter]"));
    const searchInput = document.getElementById("pr-search");
    const ciFilter = document.getElementById("ci-filter");
    const filterResult = document.getElementById("filter-result");
    const initialFilters = new URLSearchParams(window.location.search);
    let selectedStatus = initialFilters.get("status") || "";
    searchInput.value = initialFilters.get("q") || "";
    if (Array.from(ciFilter.options).some((option) => option.value === initialFilters.get("ci"))) {{
      ciFilter.value = initialFilters.get("ci");
    }}
    if (!statusButtons.some((button) => button.dataset.statusFilter === selectedStatus)) {{
      selectedStatus = "";
    }}
    statusButtons.forEach((button) => {{
      button.setAttribute("aria-pressed", String(button.dataset.statusFilter === selectedStatus));
    }});

    const syncFilterUrl = () => {{
      const url = new URL(window.location.href);
      const filters = {{ q: searchInput.value.trim(), status: selectedStatus, ci: ciFilter.value }};
      Object.entries(filters).forEach(([name, value]) => {{
        if (value) url.searchParams.set(name, value);
        else url.searchParams.delete(name);
      }});
      window.history.replaceState(null, "", url);
    }};

    const collapseDetails = (row) => {{
      const button = row.querySelector("[data-detail-target]");
      const detailRow = button ? document.getElementById(button.dataset.detailTarget) : null;
      if (button && detailRow) {{
        button.setAttribute("aria-expanded", "false");
        button.textContent = "Details";
        detailRow.hidden = true;
      }}
    }};

    dashboard.querySelectorAll("[data-detail-target]").forEach((button) => {{
      const detailRow = document.getElementById(button.dataset.detailTarget);
      button.addEventListener("click", () => {{
        const expanded = button.getAttribute("aria-expanded") === "true";
        button.setAttribute("aria-expanded", String(!expanded));
        button.textContent = expanded ? "Details" : "Hide";
        detailRow.hidden = expanded;
      }});
    }});

    const applyFilters = () => {{
      const query = searchInput.value.trim().toLocaleLowerCase();
      const selectedCi = ciFilter.value;
      const active = Boolean(query || selectedStatus || selectedCi);
      const sectionCounts = {{ open: 0, recent_closed: 0, closed_archive: 0, merged: 0 }};
      let visibleTotal = 0;

      rows.forEach((row) => {{
        const searchMatches = !query || row.dataset.search.includes(query);
        const statusMatches = !selectedStatus || row.dataset.status === selectedStatus;
        const ciMatches = !selectedCi || row.dataset.ci === selectedCi;
        const visible = searchMatches && statusMatches && ciMatches;
        row.hidden = !visible;
        if (!visible) collapseDetails(row);
        if (visible) {{
          visibleTotal += 1;
          sectionCounts[row.dataset.section] += 1;
        }}
      }});

      dashboard.querySelectorAll("[data-section-count]").forEach((counter) => {{
        counter.textContent = String(sectionCounts[counter.dataset.sectionCount] || 0);
      }});
      dashboard.querySelectorAll("[data-filter-section]").forEach((section) => {{
        const count = sectionCounts[section.dataset.filterSection] || 0;
        section.hidden = active && count === 0;
        const disclosure = section.querySelector("details");
        if (active && count > 0 && disclosure) disclosure.open = true;
      }});
      filterResult.textContent = active
        ? (visibleTotal ? "Showing " + visibleTotal + " of {total_prs} PRs" : "No matching PRs")
        : "Showing {total_prs} PRs";
    }};

    statusButtons.forEach((button) => {{
      button.addEventListener("click", () => {{
        selectedStatus = button.dataset.statusFilter;
        statusButtons.forEach((candidate) => {{
          candidate.setAttribute("aria-pressed", String(candidate === button));
        }});
        syncFilterUrl();
        applyFilters();
      }});
    }});
    searchInput.addEventListener("input", () => {{ syncFilterUrl(); applyFilters(); }});
    ciFilter.addEventListener("change", () => {{ syncFilterUrl(); applyFilters(); }});
    document.getElementById("clear-filters").addEventListener("click", () => {{
      searchInput.value = "";
      ciFilter.value = "";
      selectedStatus = "";
      statusButtons.forEach((button) => {{
        button.setAttribute("aria-pressed", String(button.dataset.statusFilter === ""));
      }});
      syncFilterUrl();
      applyFilters();
      searchInput.focus();
    }});
    applyFilters();
  </script>
</body>
</html>
"""


def write_text_atomically(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=path.parent, prefix=f".{path.name}.", delete=False
        ) as file:
            temporary_path = Path(file.name)
            file.write(value)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary_path, path)
    finally:
        if temporary_path is not None and temporary_path.exists():
            temporary_path.unlink()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="JSON emitted by pr_review.py dashboard-data")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    snapshot = json.loads(args.input.read_text(encoding="utf-8"))
    if not isinstance(snapshot, dict):
        raise ValueError("dashboard input must be a JSON object")
    write_text_atomically(args.output, render_dashboard(snapshot))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
