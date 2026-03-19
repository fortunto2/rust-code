#!/usr/bin/env python3
"""
AutoResearch Dashboard — Generic live visualization for any optimization target.

Reads results.jsonl from any target's data directory and serves a live dashboard.

Usage:
    python3 dashboard.py --target tool-selection --port 8501
    python3 dashboard.py --target system-prompt
    python3 dashboard.py --target skill-delegate
    python3 dashboard.py --target decision-parser
"""

import argparse
import json
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
from urllib.parse import parse_qs, urlparse

BASE_DIR = Path(__file__).resolve().parent / "data"


def get_target_dir(target: str) -> Path:
    return BASE_DIR / target


HTML_TEMPLATE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>AutoResearch — {target}</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4/dist/chart.umd.min.js"></script>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #faf9f7; color: #2d2a26; padding: 32px; max-width: 1400px; margin: 0 auto; }}
  .header {{ display: flex; align-items: center; gap: 16px; margin-bottom: 32px; }}
  .header h1 {{ font-size: 28px; font-weight: 700; }}
  .badge {{ background: #c0392b; color: white; font-size: 11px; font-weight: 700; padding: 3px 10px; border-radius: 4px; letter-spacing: 1px; }}
  .target-badge {{ background: #2980b9; color: white; font-size: 12px; font-weight: 600; padding: 4px 12px; border-radius: 4px; }}
  .subtitle {{ color: #8a8580; font-size: 14px; margin-top: 4px; }}
  .stats {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 32px; }}
  .stat-card {{ background: white; border-radius: 12px; padding: 20px 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.06); }}
  .stat-label {{ font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; color: #8a8580; margin-bottom: 8px; }}
  .stat-value {{ font-size: 36px; font-weight: 700; }}
  .stat-value.green {{ color: #27ae60; }}
  .stat-value.orange {{ color: #c0784a; }}
  .stat-value.neutral {{ color: #2d2a26; }}
  .chart-container {{ background: white; border-radius: 12px; padding: 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.06); margin-bottom: 32px; }}
  .chart-container canvas {{ width: 100% !important; height: 300px !important; }}
  .criteria-charts {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 16px; margin-bottom: 32px; }}
  .criteria-chart {{ background: white; border-radius: 12px; padding: 20px; box-shadow: 0 1px 3px rgba(0,0,0,0.06); }}
  .criteria-chart h3 {{ font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; color: #8a8580; margin-bottom: 12px; }}
  .criteria-chart canvas {{ width: 100% !important; height: 160px !important; }}
  .table-container {{ background: white; border-radius: 12px; padding: 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.06); margin-bottom: 32px; }}
  .table-container h3 {{ font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; color: #8a8580; margin-bottom: 16px; }}
  table {{ width: 100%; border-collapse: collapse; }}
  th {{ text-align: left; font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; color: #8a8580; padding: 8px 12px; border-bottom: 1px solid #eee; }}
  td {{ padding: 10px 12px; border-bottom: 1px solid #f5f4f2; font-size: 14px; }}
  .status-keep {{ color: #27ae60; font-weight: 600; }}
  .status-discard {{ color: #8a8580; }}
  .prompt-container {{ background: white; border-radius: 12px; padding: 24px; box-shadow: 0 1px 3px rgba(0,0,0,0.06); }}
  .prompt-container h3 {{ font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; color: #8a8580; margin-bottom: 12px; }}
  .prompt-text {{ font-family: 'SF Mono', 'Fira Code', monospace; font-size: 13px; line-height: 1.6; color: #4a4540; white-space: pre-wrap; word-break: break-word; background: #faf9f7; padding: 16px; border-radius: 8px; max-height: 400px; overflow-y: auto; }}
  @media (max-width: 768px) {{
    .stats {{ grid-template-columns: repeat(2, 1fr); }}
    .criteria-charts {{ grid-template-columns: 1fr; }}
    body {{ padding: 16px; }}
  }}
</style>
</head>
<body>
<div class="header">
  <div>
    <div style="display:flex;align-items:center;gap:12px;">
      <h1>AutoResearch</h1>
      <span class="badge" id="live-badge">LIVE</span>
      <span class="target-badge">{target}</span>
    </div>
    <div class="subtitle" id="subtitle">Refreshes every 10s</div>
  </div>
</div>

<div class="stats">
  <div class="stat-card"><div class="stat-label">Current Best</div><div class="stat-value orange" id="stat-best">—</div></div>
  <div class="stat-card"><div class="stat-label">Baseline</div><div class="stat-value neutral" id="stat-baseline">—</div></div>
  <div class="stat-card"><div class="stat-label">Improvement</div><div class="stat-value green" id="stat-improvement">—</div></div>
  <div class="stat-card"><div class="stat-label">Runs / Kept</div><div class="stat-value neutral" id="stat-runs">—</div></div>
</div>

<div class="chart-container"><canvas id="mainChart"></canvas></div>
<div class="criteria-charts" id="criteria-container"></div>
<div class="table-container">
  <h3>Run History</h3>
  <table><thead><tr id="table-header"></tr></thead><tbody id="run-table"></tbody></table>
</div>
<div class="prompt-container">
  <h3>Current Best Prompt</h3>
  <div class="prompt-text" id="best-prompt">Waiting for data...</div>
</div>

<script>
const COLORS = ['#c0784a', '#8e44ad', '#2980b9', '#27ae60', '#d35400', '#16a085', '#c0392b', '#7f8c8d'];
const LIGHT = c => c.replace(')', ', 0.12)').replace('rgb', 'rgba');
const ORANGE = '#c0784a';

let mainChart = null;
let criteriaCharts = {{}};
let knownCriteria = [];

function createChart(ctx, label, maxY, color) {{
  return new Chart(ctx, {{
    type: 'line',
    data: {{ labels: [], datasets: [{{ label, data: [], borderColor: color, backgroundColor: color.replace(')', ',0.12)').replace('#', 'rgba(').replace(/([0-9a-f]{{2}})/gi, (m) => parseInt(m,16)+','), fill: true, tension: 0.3, pointRadius: 5, pointBackgroundColor: [], pointBorderColor: color, pointBorderWidth: 2 }}] }},
    options: {{
      responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ display: false }} }},
      scales: {{
        x: {{ grid: {{ display: false }}, ticks: {{ font: {{ size: 11 }}, color: '#8a8580' }} }},
        y: {{ grid: {{ color: '#f0efed' }}, min: 0, max: maxY, ticks: {{ font: {{ size: 11 }}, color: '#8a8580', stepSize: maxY <= 10 ? 1 : 5 }} }}
      }}
    }}
  }});
}}

function hexToRgba(hex, alpha) {{
  const r = parseInt(hex.slice(1,3), 16);
  const g = parseInt(hex.slice(3,5), 16);
  const b = parseInt(hex.slice(5,7), 16);
  return `rgba(${{r}},${{g}},${{b}},${{alpha}})`;
}}

function initMainChart(maxScore) {{
  if (mainChart) mainChart.destroy();
  const ctx = document.getElementById('mainChart').getContext('2d');
  mainChart = new Chart(ctx, {{
    type: 'line',
    data: {{ labels: [], datasets: [{{ label: 'Score', data: [], borderColor: ORANGE, backgroundColor: hexToRgba(ORANGE, 0.15), fill: true, tension: 0.3, pointRadius: 5, pointBackgroundColor: [], pointBorderColor: ORANGE, pointBorderWidth: 2 }}] }},
    options: {{
      responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ display: false }} }},
      scales: {{
        x: {{ grid: {{ display: false }}, ticks: {{ font: {{ size: 11 }}, color: '#8a8580' }} }},
        y: {{ grid: {{ color: '#f0efed' }}, min: 0, max: maxScore, ticks: {{ font: {{ size: 11 }}, color: '#8a8580', stepSize: maxScore <= 10 ? 1 : 5 }} }}
      }}
    }}
  }});
}}

function initCriteriaCharts(criteria, batchSize) {{
  const container = document.getElementById('criteria-container');
  container.innerHTML = '';
  criteriaCharts = {{}};
  criteria.forEach((c, i) => {{
    const div = document.createElement('div');
    div.className = 'criteria-chart';
    div.innerHTML = `<h3>${{c.replace(/_/g, ' ')}}</h3><canvas id="chart-${{c}}"></canvas>`;
    container.appendChild(div);
    const ctx = div.querySelector('canvas').getContext('2d');
    const color = COLORS[i % COLORS.length];
    criteriaCharts[c] = new Chart(ctx, {{
      type: 'line',
      data: {{ labels: [], datasets: [{{ label: c, data: [], borderColor: color, backgroundColor: hexToRgba(color, 0.12), fill: true, tension: 0.3, pointRadius: 4, pointBackgroundColor: color, pointBorderColor: color, pointBorderWidth: 1 }}] }},
      options: {{
        responsive: true, maintainAspectRatio: false,
        plugins: {{ legend: {{ display: false }} }},
        scales: {{
          x: {{ grid: {{ display: false }}, ticks: {{ font: {{ size: 10 }}, color: '#8a8580' }} }},
          y: {{ grid: {{ color: '#f0efed' }}, min: 0, max: batchSize, ticks: {{ font: {{ size: 10 }}, color: '#8a8580', stepSize: 1 }} }}
        }}
      }}
    }});
  }});
}}

function formatTime(iso) {{
  if (!iso) return '';
  return new Date(iso).toLocaleTimeString([], {{ hour: '2-digit', minute: '2-digit' }});
}}

async function refresh() {{
  let data;
  try {{ data = await (await fetch('/api/data')).json(); }} catch(e) {{ return; }}
  if (!data.runs || !data.runs.length) return;

  const runs = data.runs;
  const maxScore = runs[0].max || 40;
  const labels = runs.map(r => r.run);
  const scores = runs.map(r => r.score);
  const baseline = scores[0];
  const best = Math.max(...scores);

  // Detect criteria from first run
  const criteria = Object.keys(runs[0].criteria || {{}});
  const batchSize = runs[0].generated || 10;

  if (JSON.stringify(criteria) !== JSON.stringify(knownCriteria)) {{
    knownCriteria = criteria;
    initMainChart(maxScore);
    initCriteriaCharts(criteria, batchSize);
    // Build table header
    const th = document.getElementById('table-header');
    th.innerHTML = '<th>Run</th><th>Status</th><th>Score</th>' + criteria.map(c => `<th>${{c.replace(/_/g,' ')}}</th>`).join('') + '<th>Time</th>';
  }}

  // Stats
  document.getElementById('stat-best').textContent = best + '/' + maxScore;
  document.getElementById('stat-baseline').textContent = baseline + '/' + maxScore;
  const improv = baseline > 0 ? ((best - baseline) / baseline * 100).toFixed(1) : '—';
  const improvEl = document.getElementById('stat-improvement');
  improvEl.textContent = improv === '—' ? '—' : (improv > 0 ? '+' : '') + improv + '%';
  improvEl.className = 'stat-value ' + (improv > 0 ? 'green' : improv < 0 ? 'orange' : 'neutral');

  let kept = 0, rb = -1;
  scores.forEach(s => {{ if (s > rb) {{ kept++; rb = s; }} }});
  document.getElementById('stat-runs').textContent = runs.length + ' / ' + kept;

  // Main chart
  mainChart.data.labels = labels;
  mainChart.data.datasets[0].data = scores;
  let rb2 = -1;
  mainChart.data.datasets[0].pointBackgroundColor = scores.map(v => {{ if (v > rb2) {{ rb2 = v; return ORANGE; }} return '#c4c0bb'; }});
  mainChart.update('none');

  // Criteria charts
  criteria.forEach(c => {{
    if (!criteriaCharts[c]) return;
    criteriaCharts[c].data.labels = labels;
    criteriaCharts[c].data.datasets[0].data = runs.map(r => r.criteria?.[c] ?? 0);
    criteriaCharts[c].update('none');
  }});

  // Table
  let rb3 = -1;
  const statuses = scores.map(s => {{ if (s > rb3) {{ rb3 = s; return 'keep'; }} return 'discard'; }});
  const rows = runs.map((r, idx) => {{
    const st = statuses[idx];
    const critCells = criteria.map(c => `<td>${{r.criteria?.[c] ?? '?'}}/${{batchSize}}</td>`).join('');
    return `<tr><td>${{r.run}}</td><td class="status-${{st}}">${{st}}</td><td><strong>${{r.score}}/${{maxScore}}</strong></td>${{critCells}}<td>${{formatTime(r.timestamp)}}</td></tr>`;
  }}).reverse();
  document.getElementById('run-table').innerHTML = rows.join('');

  if (data.best_prompt) document.getElementById('best-prompt').textContent = data.best_prompt;
  document.getElementById('subtitle').textContent = `${{runs.length}} runs — best ${{best}}/${{maxScore}} — last: ${{formatTime(runs[runs.length-1]?.timestamp)}}`;
}}

refresh();
setInterval(refresh, 10000);
</script>
</body>
</html>"""


class DashboardHandler(SimpleHTTPRequestHandler):
    target_dir: Path = BASE_DIR
    target_name: str = "unknown"

    def do_GET(self):
        parsed = urlparse(self.path)

        if parsed.path in ("/", "/index.html"):
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            html = HTML_TEMPLATE.format(target=self.target_name)
            self.wfile.write(html.encode())

        elif parsed.path == "/api/data":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()

            results_file = self.target_dir / "results.jsonl"
            best_file = self.target_dir / "best_prompt.txt"

            runs = []
            if results_file.exists():
                for line in results_file.read_text().strip().split("\n"):
                    if line.strip():
                        try:
                            runs.append(json.loads(line))
                        except json.JSONDecodeError:
                            pass

            best_prompt = best_file.read_text().strip() if best_file.exists() else ""
            self.wfile.write(json.dumps({"runs": runs, "best_prompt": best_prompt}).encode())

        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass


def main():
    parser = argparse.ArgumentParser(description="AutoResearch Dashboard")
    parser.add_argument("--target", required=True, help="Target name (tool-selection, system-prompt, skill-X, decision-parser)")
    parser.add_argument("--port", type=int, default=8501)
    args = parser.parse_args()

    target_dir = get_target_dir(args.target)
    if not target_dir.exists():
        print(f"WARNING: Data directory not found: {target_dir}")
        print("Start a runner first to generate data.")
        target_dir.mkdir(parents=True, exist_ok=True)

    DashboardHandler.target_dir = target_dir
    DashboardHandler.target_name = args.target

    server = HTTPServer(("0.0.0.0", args.port), DashboardHandler)
    print(f"AutoResearch Dashboard: {args.target}")
    print(f"  URL: http://localhost:{args.port}")
    print(f"  Data: {target_dir}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nShutdown.")


if __name__ == "__main__":
    main()
