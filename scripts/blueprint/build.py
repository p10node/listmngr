#!/usr/bin/env python3
"""Render docs/PLAN.md into the listmngr Blueprint artifact page.

    pandoc docs/PLAN.md -f gfm+task_lists -t html5 -s --toc --toc-depth=3 \
      --template scripts/blueprint/template.pandoc -o BUILD/raw.html
    python3 scripts/blueprint/build.py BUILD MAIN_SHORT_SHA

Writes BUILD/listmngr-blueprint.html: page content only (no html/head/body
wrapper), to publish with the Artifact tool to the blueprint's existing URL.
The shell files hold the page's head, styles, mast and script.
"""
import re, sys, pathlib
S = pathlib.Path(sys.argv[1])  # build directory holding raw.html
raw = (S/'raw.html').read_text()
HERE = pathlib.Path(__file__).resolve().parent

toc = raw.split('<!--TOC-->')[1].split('<!--/TOC-->')[0].strip()
body = raw.split('<!--BODY-->')[1].split('<!--/BODY-->')[0].strip()

# Drop the H1 entry from the TOC and the H1 + lede blockquote from the body.
toc = re.sub(r'^<ul>\s*<li><a href="#listmngr[^"]*"[^>]*>.*?</a>\s*<ul>', '<ul>', toc, count=1, flags=re.S)
toc = re.sub(r'</ul>\s*</li>\s*</ul>\s*$', '</ul>', toc, flags=re.S)
body = re.sub(r'^<h1[^>]*>.*?</h1>\s*<blockquote>.*?</blockquote>\s*', '', body, count=1, flags=re.S)

# Tables scroll in their own container.
body = body.replace('<table>', '<div class="tbl"><table>').replace('</table>', '</table></div>')

# Phase chips inside table cells, never inside code and never in an ID like P4-SHELL.
def chip_cell(m):
    cell = m.group(0)
    parts = re.split(r'(<code>.*?</code>)', cell, flags=re.S)
    for i, part in enumerate(parts):
        if part.startswith('<code>'):
            continue
        parts[i] = re.sub(r'\bP([0-7])\b(?!-)', r'<span class="ph ph\1">P\1</span>', part)
    return ''.join(parts)
body = re.sub(r'<td>.*?</td>', chip_cell, body, flags=re.S)

# Legend under the parity matrix intro (the first paragraph after the §4 heading).
legend = ('<div class="legend"><span>Phase</span>' + ''.join(f'<span class="ph ph{i}">P{i}</span>' for i in range(8)) + '<span>= phase expected to complete it</span></div>')
body = re.sub(r'(<h2 id="4-feature-parity-matrix">.*?</p>)', r'\1' + legend, body, count=1, flags=re.S)

# Work-package ids in the first column read as identifiers.
body = re.sub(r'<td>(P[0-7]-[A-Z0-9-]+)</td>', r'<td><code class="wp">\1</code></td>', body)

head = (HERE/'shell-head.html').read_text()
tail = (HERE/'shell-tail.html').read_text()

mast = '''<div class="airmail"></div>
<div class="page">
<header class="mast">
  <div>
    <p class="eyebrow">docs/PLAN.md · main @SHA@</p>
    <h1>listmngr<small>Mailman 3 alternative written in Rust</small></h1>
    <p class="lede">One binary in place of mailman-core, Postorius, HyperKitty and mailman-web. Same features and concept names; a different runtime, UI and security model.</p>
  </div>
  <pre class="hdrs"><b>Status:</b>   Phase 0–10 closed · 1.2.1 is the fourth release
<b>Date:</b>     Sat, 31 Oct 2026
<b>Replaces:</b> mailman-core, postorius, hyperkitty,
          mailman-web, django-mailman3
<b>Stack:</b>    Rust 2024 · tokio · axum · sqlx
          askama + htmx · tantivy · mail-auth
<b>Roadmap:</b>  <em>P0–P10</em> done → backlog after 1.1 (§7, Phase 8)
<b>Gates:</b>    1192 tests · PG 101/101 · mailmanclient PASS
<b>List-Id:</b>  &lt;plan.listmngr&gt;</pre>
</header>
<nav class="toc" id="TOC" aria-label="Table of contents">''' + toc + '''</nav><main>
''' + body + '''
</main></div>'''
out = (head + mast + tail).replace('@SHA@', sys.argv[2])
(S/'listmngr-blueprint.html').write_text(out)
print(len(out), 'bytes;', out.count('<table'), 'tables;', out.count('class="ph ph'), 'chips;', out.count('checked=""'), 'ticked')
