"""Disposable Chromium acceptance journey at a phone viewport.

One person, in order: sign up, verify the mailbox, sign in, create a list,
subscribe an address to it (mailbox confirmation), see the first post held,
accept it from the moderation page, change a list setting, sign out. Every
stop is checked for horizontal overflow and, when axe-core is passed in, for
critical or serious accessibility violations. With WEBUI_LIGHTHOUSE set to a
lighthouse executable, four pages are audited for an accessibility score of
at least 95. No cookie, password or token is written to the evidence folder.
"""
import json
import os
import subprocess
import time
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

base = os.environ['WEBUI_URL']
out = Path(os.environ['WEBUI_OUTPUT'])
bridge = Path(os.environ['WEBUI_BRIDGE_DIR'])
password = os.environ['WEBUI_TEST_PASSWORD']
axe_path = os.environ.get('WEBUI_AXE_SCRIPT')
axe_source = Path(axe_path).read_text(encoding='utf-8') if axe_path else None
lighthouse = os.environ.get('WEBUI_LIGHTHOUSE')
errors = []
scanned = []
stops = 0


def wait_for(path, seconds=60):
    """A file the Rust harness writes once a mail or a database row exists."""
    deadline = time.time() + seconds
    while time.time() < deadline:
        if path.exists():
            text = path.read_text(encoding='utf-8').strip()
            if text:
                return text
        time.sleep(0.2)
    raise AssertionError(f'bridge did not deliver {path.name}')


def stop(page, label, screenshot=True):
    """A journey stop: no horizontal overflow at phone width, no blocking axe violation.

    A stop whose page shows a one-time token takes no screenshot."""
    global stops
    stops += 1
    assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), f'horizontal overflow on {label}'
    if axe_source is not None:
        page.evaluate(axe_source)
        report = page.evaluate("async () => await axe.run(document, {resultTypes: ['violations']})")
        blocking = [f"{v['id']} ({v['impact']}) on {label}" for v in report['violations'] if v['impact'] in ('critical', 'serious')]
        assert not blocking, blocking
        scanned.append(label)
    if screenshot:
        page.screenshot(path=str(out / f'journey-{stops:02d}.png'), full_page=True)


def audit(url, cookie, label, chrome):
    """Lighthouse accessibility, mobile form factor, on one page."""
    report = out / f'lighthouse-{label}.json'
    command = [
        lighthouse, url, '--quiet', '--only-categories=accessibility', '--output=json',
        f'--output-path={report}', '--chrome-flags=--headless=new --no-sandbox',
    ]
    if cookie:
        command.append('--extra-headers=' + json.dumps({'Cookie': cookie}))
    env = dict(os.environ, CHROME_PATH=chrome)
    subprocess.run(command, check=True, env=env, timeout=180)
    data = json.loads(report.read_text(encoding='utf-8'))
    score = data['categories']['accessibility']['score']
    # The report names no cookie; the audited pages carry no secret either.
    report.write_text(json.dumps({'url': url, 'accessibility': score}), encoding='utf-8')
    assert score is not None and score >= 0.95, f'{label}: accessibility {score}'
    return score


with sync_playwright() as p:
    executable = os.environ.get('WEBUI_CHROMIUM_EXECUTABLE')
    browser = p.chromium.launch(executable_path=executable, headless=True)
    context = browser.new_context(viewport={'width': 390, 'height': 844}, device_scale_factor=2)
    page = context.new_page()
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.on('console', lambda msg: errors.append(msg.text) if msg.type == 'error' else None)

    # 1. Sign up.
    page.goto(base + '/web/signup')
    page.get_by_label('Email', exact=True).fill('journey@example.com')
    page.get_by_label('Display name', exact=True).fill('Journey Person')
    page.get_by_label('Password', exact=True).fill(password)
    page.get_by_label('Confirm password', exact=True).fill(password)
    stop(page, 'signup')
    page.get_by_role('button', name='Create account', exact=True).click()
    expect(page.get_by_role('heading', name='Check your email', exact=True)).to_be_visible()

    # 2. Verify the mailbox with the token the harness read from the mail.
    token = wait_for(bridge / 'token-1')
    page.goto(base + '/web/verify?token=' + token)
    expect(page.get_by_label('Token from email', exact=True)).to_have_value(token)
    stop(page, 'verify', screenshot=False)
    page.get_by_role('button', name='Verify address', exact=True).click()
    expect(page.get_by_role('heading', name='Address verified', exact=True)).to_be_visible()

    # 3. Sign in.
    page.goto(base + '/web/login')
    page.get_by_label('Email', exact=True).fill('journey@example.com')
    page.get_by_label('Password', exact=True).fill(password)
    stop(page, 'login')
    page.get_by_role('button', name='Log in', exact=True).click()
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()

    # 4. Create a list, once the harness has seated the account as a domain owner.
    wait_for(bridge / 'owner-granted')
    page.goto(base + '/web/lists/new')
    expect(page.get_by_label('Owner address', exact=True)).to_have_value('journey@example.com')
    page.get_by_label('List name', exact=True).fill('journey')
    page.get_by_label('Domain', exact=True).select_option('example.com')
    page.get_by_label('Display name', exact=True).fill('Journey list')
    page.get_by_label('Description', exact=True).fill('Made on the journey')
    stop(page, 'create-list')
    page.get_by_role('button', name='Create the list', exact=True).click()
    expect(page).to_have_url(base + '/web/lists/journey.example.com/settings')
    expect(page.get_by_role('heading', name='List settings', exact=True)).to_be_visible()
    stop(page, 'settings-after-create')

    # 5. Subscribe an address through the public list page and confirm it.
    page.goto(base + '/web/lists/journey.example.com')
    expect(page.get_by_role('heading', name='Journey list', exact=True)).to_be_visible()
    page.get_by_label('Email', exact=True).fill('friend@example.org')
    stop(page, 'list-summary')
    page.get_by_role('button', name='Send confirmation instructions', exact=True).click()
    expect(page.get_by_role('heading', name='Check your email', exact=True)).to_be_visible()
    token = wait_for(bridge / 'token-2')
    page.goto(base + '/web/lists/journey.example.com/confirm?token=' + token)
    stop(page, 'confirm', screenshot=False)
    page.get_by_role('button', name='Confirm request', exact=True).click()
    expect(page.get_by_role('heading', name='Request confirmed', exact=True)).to_be_visible()

    # 6. The first post is held; accept it from the moderation page.
    wait_for(bridge / 'post-held')
    page.goto(base + '/web/moderation')
    expect(page.locator('article[data-held]')).to_have_count(1)
    expect(page.locator('article[data-held] .list-id')).to_contain_text('journey.example.com')
    stop(page, 'moderation')
    decision = page.locator('article[data-held] form.decision').first
    decision.locator('select[name="action"]').select_option('accept')
    decision.get_by_role('button', name='Apply decision', exact=True).click()
    expect(page).to_have_url(base + '/web/moderation?done=1')
    expect(page.get_by_role('status')).to_contain_text('1 decided')
    expect(page.locator('article[data-held]')).to_have_count(0)

    # 7. Change a setting.
    page.goto(base + '/web/lists/journey.example.com/settings/identity')
    page.get_by_label('Description', exact=True).fill('Set during the journey')
    stop(page, 'settings-identity')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Saved')
    expect(page.get_by_label('Description', exact=True)).to_have_value('Set during the journey')

    # 8. Lighthouse, with and without the session, before signing out.
    scores = {}
    if lighthouse:
        cookie = '; '.join(f"{c['name']}={c['value']}" for c in context.cookies() if c['name'] == 'listmngr_session')
        chrome = executable or p.chromium.executable_path
        for label, path, with_cookie in [
            ('directory', '/web', False),
            ('login', '/web/login', False),
            ('list', '/web/lists/journey.example.com', False),
            ('settings', '/web/lists/journey.example.com/settings/identity', True),
        ]:
            scores[label] = audit(base + path, cookie if with_cookie else None, label, chrome)

    # 9. Sign out.
    page.goto(base + '/web/account')
    stop(page, 'account')
    page.get_by_role('button', name='Log out', exact=True).click()
    expect(page.get_by_role('heading', name='Mailing lists', exact=True)).to_be_visible()
    assert not any(c['name'] == 'listmngr_session' for c in context.cookies())
    stop(page, 'directory-after-logout')

    assert not errors, errors
    axe_note = f'axe: no critical or serious violation on {len(scanned)} stops' if axe_source else 'axe skipped'
    lighthouse_note = ('lighthouse accessibility ' + ', '.join(f'{k}={v:.2f}' for k, v in scores.items())) if scores else 'lighthouse skipped'
    print(f'JOURNEY PASS ({browser.version}) at 390x844: signup, verify, login, create list, subscribe and confirm, held post accepted from the moderation page, setting saved, logout; {stops} stops without horizontal overflow; {axe_note}; {lighthouse_note}; zero console or page errors.', flush=True)
    context.close()
    browser.close()
