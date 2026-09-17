"""Disposable Chromium acceptance. No cookies/passwords/tokens written to evidence."""
import base64
import re
import hashlib
import hmac
import os
import mailbox
import struct
import time
from pathlib import Path
from playwright.sync_api import sync_playwright, expect


def totp_code(secret_base32, step_offset=0):
    """RFC 6238 with SHA-1, six digits and 30-second steps, as an app would."""
    key = base64.b32decode(secret_base32 + '=' * (-len(secret_base32) % 8))
    counter = int(time.time()) // 30 + step_offset
    digest = hmac.new(key, struct.pack('>q', counter), hashlib.sha1).digest()
    offset = digest[-1] & 0x0F
    binary = struct.unpack('>I', digest[offset:offset + 4])[0] & 0x7FFFFFFF
    return f'{binary % 1_000_000:06d}'


def second_step(page, code):
    """The page after a correct password on an enrolled account."""
    expect(page.get_by_role('heading', name='Enter your one-time code', exact=True)).to_be_visible()
    page.get_by_label('Code', exact=True).fill(code)
    page.get_by_role('button', name='Finish signing in', exact=True).click()

base = os.environ['WEBUI_URL']
out = Path(os.environ['WEBUI_OUTPUT'])
errors = []
# axe-core is a development-only scanner, so it is passed in rather than
# vendored; `page.evaluate` is not subject to the page CSP.
axe_path = os.environ.get('WEBUI_AXE_SCRIPT')
axe_source = Path(axe_path).read_text(encoding='utf-8') if axe_path else None
scanned = []


def axe_scan(page, label):
    """Fail on any critical or serious violation of the page as it stands."""
    if axe_source is None:
        return
    page.evaluate(axe_source)
    report = page.evaluate("async () => await axe.run(document, {resultTypes: ['violations']})")
    blocking = [
        f"{violation['id']} ({violation['impact']}) on {label}"
        for violation in report['violations']
        if violation['impact'] in ('critical', 'serious')
    ]
    assert not blocking, blocking
    scanned.append(label)

with sync_playwright() as p:
    executable = os.environ.get('WEBUI_CHROMIUM_EXECUTABLE')
    browser = p.chromium.launch(executable_path=executable, headless=True)
    context = browser.new_context(viewport={'width': 1280, 'height': 900})
    page = context.new_page()
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.on('response', lambda response: print('HTTP', response.status, response.url.split('?')[0], flush=True))
    # A form re-rendered with its refusals inline answers 400, which Chromium
    # logs as a resource error; each deliberate one is announced first.
    expected_refusals = {'count': 0}

    def on_console(msg):
        if msg.type != 'error':
            return
        if expected_refusals['count'] > 0 and 'status of 400' in msg.text:
            expected_refusals['count'] -= 1
            return
        errors.append(msg.text)

    def expect_refusal():
        expected_refusals['count'] += 1

    page.on('console', on_console)
    page.goto(base + '/web')
    expect(page.get_by_role('heading', name='Mailing lists')).to_be_visible()
    expect(page.locator('main')).not_to_contain_text('private.example.com')
    assert page.locator('script').count() == 0
    assert page.locator('h1').evaluate('(e) => getComputedStyle(e).fontSize') != '32px', 'local CSS loaded'
    page.screenshot(path=str(out / '01-directory.png'), full_page=True)
    page.goto(base + '/web/login')
    page.get_by_label('Email', exact=True).fill('browser@example.com')
    page.get_by_label('Password', exact=True).fill(os.environ['WEBUI_TEST_PASSWORD'])
    page.get_by_role('button', name='Log in', exact=True).click()
    expect(page.get_by_role('heading', name='My subscriptions')).to_be_visible()
    # P4-TOTP: the site requires a second factor of a server owner; until one
    # is enrolled the administration pages answer 403.
    expect(page.locator('main')).to_contain_text('requires a second sign-in step')
    closed = context.request.get(base + '/web/admin')
    assert closed.status == 403, 'administration stays closed before enrolment'
    page.get_by_role('link', name='Two-step sign-in', exact=True).first.click()
    expect(page.get_by_role('heading', name='Two-step sign-in', exact=True)).to_be_visible()
    totp_secret = page.locator('#totp-secret').inner_text()
    assert len(totp_secret) == 32
    assert page.locator('svg').count() == 1, 'an inline QR code'
    page.get_by_label('Six-digit code from the app', exact=True).fill(totp_code(totp_secret))
    page.get_by_role('button', name='Turn on two-step sign-in', exact=True).click()
    recovery_codes = [c.inner_text() for c in page.locator('li code').all()]
    assert len(recovery_codes) == 10, recovery_codes
    # Screenshots must not carry live recovery codes into the evidence folder.
    page.evaluate("document.querySelectorAll('li code').forEach(c => c.textContent = 'redacted')")
    page.screenshot(path=str(out / '21-totp-recovery-codes.png'), full_page=True)
    # P4-WEBAUTHN: a virtual authenticator (CDP) stands in for the device; the
    # page's own script runs the ceremony and the key is listed afterwards.
    cdp = context.new_cdp_session(page)
    cdp.send('WebAuthn.enable')
    virtual = cdp.send('WebAuthn.addVirtualAuthenticator', {'options': {
        'protocol': 'ctap2', 'transport': 'internal', 'hasResidentKey': True,
        'hasUserVerification': True, 'isUserVerified': True, 'automaticPresenceSimulation': True,
    }})['authenticatorId']
    page.goto(base + '/web/account/passkeys')
    expect(page.get_by_role('heading', name='Passkeys', exact=True)).to_be_visible()
    assert page.locator('script[src="/web/passkeys.js"]').count() == 1, 'the one first-party script'
    page.get_by_label('Name for this passkey', exact=True).fill('Virtual authenticator')
    page.get_by_role('button', name='Add a passkey', exact=True).click()
    expect(page.get_by_role('heading', name='Virtual authenticator', exact=True)).to_be_visible()
    stored = cdp.send('WebAuthn.getCredentials', {'authenticatorId': virtual})['credentials']
    assert len(stored) == 1 and stored[0]['isResidentCredential'], stored
    page.screenshot(path=str(out / '22-passkeys.png'), full_page=True)
    page.goto(base + '/web/account')
    page.get_by_role('link', name='List administration', exact=True).click()
    expect(page.get_by_role('heading', name='List administration', exact=True)).to_be_visible()
    page.locator('a[href="/web/lists/public.example.com/settings"]').click()
    expect(page.get_by_role('heading', name='List settings', exact=True)).to_be_visible()
    original_name = page.get_by_label('Display name', exact=True).input_value()
    prefix = '  "<script>& Tiếng Việt + ✉  '
    page.get_by_label('Subject prefix', exact=True).fill(prefix)
    page.get_by_role('button', name='Save list settings', exact=True).click()
    page.reload()
    expect(page.get_by_label('Subject prefix', exact=True)).to_have_value(prefix)
    assert page.locator('script').count() == 0
    page.screenshot(path=str(out / '15-subject-prefix.png'), full_page=True)
    page.get_by_label('Subject prefix', exact=True).fill('')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    page.reload()
    expect(page.get_by_label('Subject prefix', exact=True)).to_have_value('')
    print('Subject prefix native save/reload/escaped DOM/explicit clear: PASS', flush=True)
    page.get_by_label('Display name', exact=True).fill('Chromium settings <saved> & verified')
    page.get_by_label('Description', exact=True).fill('Browser description </textarea><script>bad</script>')
    page.get_by_label('Show in public directory', exact=True).select_option('false')
    page.get_by_label('Default member posting action', exact=True).select_option('hold')
    page.get_by_label('Default nonmember posting action', exact=True).select_option('reject')
    page.get_by_label('Archive policy', exact=True).select_option('private')
    page.get_by_label('Maximum message size (KiB)', exact=True).fill('256')
    page.get_by_label('To/Cc recipient hold threshold', exact=True).fill('17')
    page.get_by_label('Send welcome messages', exact=True).select_option('true')
    page.get_by_label('Send goodbye messages', exact=True).select_option('true')
    page.get_by_label('Emergency moderation', exact=True).select_option('true')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    page.reload()
    expect(page.get_by_label('Display name', exact=True)).to_have_value('Chromium settings <saved> & verified')
    expect(page.get_by_label('Description', exact=True)).to_have_value('Browser description </textarea><script>bad</script>')
    expect(page.get_by_label('Show in public directory', exact=True)).to_have_value('false')
    expect(page.get_by_label('Default member posting action', exact=True)).to_have_value('hold')
    expect(page.get_by_label('Default nonmember posting action', exact=True)).to_have_value('reject')
    expect(page.get_by_label('Archive policy', exact=True)).to_have_value('private')
    expect(page.get_by_label('Maximum message size (KiB)', exact=True)).to_have_value('256')
    expect(page.get_by_label('To/Cc recipient hold threshold', exact=True)).to_have_value('17')
    expect(page.get_by_label('Send welcome messages', exact=True)).to_have_value('true')
    expect(page.get_by_label('Send goodbye messages', exact=True)).to_have_value('true')
    expect(page.get_by_label('Emergency moderation', exact=True)).to_have_value('true')
    assert page.locator('script').count() == 0
    page.screenshot(path=str(out / '14-list-settings.png'), full_page=True)
    # Restore only fields required by the existing composed archive/directory flow.
    page.get_by_label('Display name', exact=True).fill(original_name)
    page.get_by_label('Show in public directory', exact=True).select_option('true')
    page.get_by_label('Archive policy', exact=True).select_option('public')
    page.get_by_label('Maximum message size (KiB)', exact=True).fill('0')
    page.get_by_label('To/Cc recipient hold threshold', exact=True).fill('0')
    page.get_by_label('Send welcome messages', exact=True).select_option('false')
    page.get_by_label('Send goodbye messages', exact=True).select_option('false')
    page.get_by_label('Emergency moderation', exact=True).select_option('false')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    # Settings groups: a preview that writes nothing, an inline refusal, a save.
    page.get_by_role('link', name='Message acceptance', exact=True).click()
    expect(page.get_by_role('heading', name='Message acceptance', exact=True)).to_be_visible()
    axe_scan(page, '/web/lists/public.example.com/settings/acceptance')
    page.get_by_label('Hold posts that look like commands', exact=True).select_option('false')
    page.get_by_role('button', name='Preview changes', exact=True).click()
    expect(page.get_by_role('heading', name='What would change', exact=True)).to_be_visible()
    expect(page.locator('table')).to_contain_text('administrivia')
    page.screenshot(path=str(out / '30-settings-preview.png'), full_page=True)
    page.get_by_label('Maximum message size (KiB)', exact=True).fill('-1')
    expect_refusal()
    page.get_by_role('button', name='Preview changes', exact=True).click()
    expect(page.locator('#max_message_size-error')).to_be_visible()
    page.get_by_label('Maximum message size (KiB)', exact=True).fill('0')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Saved')
    expect(page.get_by_label('Hold posts that look like commands', exact=True)).to_have_value('false')
    page.get_by_label('Hold posts that look like commands', exact=True).select_option('true')
    page.get_by_role('button', name='Save list settings', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Saved')
    # Header rules: add, test a value, remove.
    page.get_by_role('link', name='Header rules', exact=True).click()
    expect(page.get_by_role('heading', name='Header rules', exact=True)).to_be_visible()
    page.locator('#header').fill('X-Spam-Flag')
    page.locator('#pattern').fill('^YES')
    page.locator('#action').select_option('discard')
    page.locator('#tag').fill('spam')
    page.get_by_role('button', name='Add rule', exact=True).click()
    expect(page.locator('section.rule')).to_have_count(1)
    expect(page.locator('section.rule h3')).to_contain_text('X-Spam-Flag')
    axe_scan(page, '/web/lists/public.example.com/settings/header-matches')
    page.locator('#test-header').fill('x-spam-flag')
    page.locator('#test-value').fill('YES')
    page.get_by_role('button', name='Test', exact=True).click()
    expect(page.get_by_role('heading', name='Test result', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('Rule 1 matches')
    page.screenshot(path=str(out / '31-header-rules.png'), full_page=True)
    page.get_by_role('button', name='Remove rule', exact=True).click()
    expect(page.locator('section.rule')).to_have_count(0)
    # Bans: add and lift.
    page.get_by_role('link', name='Bans', exact=True).click()
    page.get_by_label('Address or pattern', exact=True).fill('Banned-Browser@Example.org')
    page.get_by_role('button', name='Ban', exact=True).click()
    expect(page.locator('main')).to_contain_text('banned-browser@example.org')
    page.get_by_role('button', name='Lift ban', exact=True).click()
    expect(page.locator('main')).to_contain_text('Nobody is banned')
    # Templates: preview with placeholders, save, remove.
    page.get_by_role('link', name='Templates', exact=True).click()
    page.get_by_role('link', name='list:user:notice:welcome', exact=True).click()
    page.get_by_label('Text', exact=True).fill('Hello $display_name from Chromium <$listname>')
    page.get_by_role('button', name='Preview', exact=True).click()
    expect(page.locator('main')).to_contain_text('from Chromium <public@example.com>')
    page.screenshot(path=str(out / '32-template-editor.png'), full_page=True)
    page.get_by_role('button', name='Save text', exact=True).click()
    expect(page.locator('main')).to_contain_text('This list stores its own text')
    page.get_by_role('button', name="Remove this list's text (every language)", exact=True).click()
    expect(page.locator('main')).to_contain_text('inherited')
    # Deleting needs the id typed back; a wrong one is refused inline.
    page.get_by_role('link', name='Delete list', exact=True).click()
    page.get_by_label('Type the list id to confirm:').fill('nope.example.com')
    expect_refusal()
    page.get_by_role('button', name='Delete the list', exact=True).click()
    expect(page.get_by_role('alert')).to_contain_text('does not match')
    assert expected_refusals['count'] == 0, 'every announced refusal was logged'
    print('Settings groups/rules/bans/templates/delete confirmation: PASS', flush=True)
    page.goto(base + '/web/admin')
    page.locator('a[href="/web/lists/public.example.com/members"]').click()
    axe_scan(page, '/web/lists/public.example.com/members')
    # The search is an htmx swap of the roster: the page is not reloaded
    # (a marker set on the document survives) and the URL is pushed.
    page.evaluate("document.body.dataset.marker = 'kept'")
    page.get_by_label('Search member email', exact=True).fill('BROWSER@EXAMPLE.COM')
    page.get_by_role('button', name='Search members', exact=True).click()
    expect(page.locator('article')).to_have_count(1)
    expect(page).to_have_url(re.compile(r'q=BROWSER'))
    assert page.evaluate("document.body.dataset.marker") == 'kept', 'htmx swapped the roster in place'
    admin_member = page.locator('article').filter(has=page.get_by_role('heading', name='browser@example.com', exact=True))
    admin_member.get_by_label('Posting policy', exact=True).select_option('hold')
    admin_member.get_by_role('button', name='Save posting policy', exact=True).click()
    page.reload()
    expect(admin_member.get_by_label('Posting policy', exact=True)).to_have_value('hold')
    expect(page.get_by_label('Search member email', exact=True)).to_have_value('BROWSER@EXAMPLE.COM')
    page.screenshot(path=str(out / '13-member-policy.png'), full_page=True)
    admin_member.get_by_label('Posting policy', exact=True).select_option('default')
    admin_member.get_by_role('button', name='Save posting policy', exact=True).click()
    page.reload()
    expect(admin_member.get_by_label('Posting policy', exact=True)).to_have_value('default')
    expect(page.get_by_label('Search member email', exact=True)).to_have_value('BROWSER@EXAMPLE.COM')
    page.get_by_label('Search member email', exact=True).fill('no-such-member')
    page.get_by_role('button', name='Search members', exact=True).click()
    expect(page.locator('article')).to_have_count(0)
    expect(page.locator('main')).to_contain_text('No matching members.')
    page.get_by_role('link', name='Clear search', exact=True).click()
    expect(page.get_by_label('Search member email', exact=True)).to_have_value('')
    expect(admin_member).to_be_visible()
    # Rosters of the other roles, mass subscription, one member's options,
    # bounce reset, removal and the export.
    page.get_by_role('link', name='Owners', exact=True).click()
    expect(page).to_have_url(re.compile(r'role=owner'))
    expect(page.locator('main')).to_contain_text('No matching members.')  # the fixture owner is a server owner, not a list owner
    page.get_by_role('link', name='Members', exact=True).click()
    page.get_by_role('link', name='Add members', exact=True).click()
    expect(page.get_by_role('heading', name='Add members', exact=True)).to_be_visible()
    page.get_by_label('Addresses', exact=True).fill('Chromium One <chromium-one@example.org>\nchromium-two@example.org\nnot-an-address')
    page.get_by_label('The people have asked to join', exact=True).check()
    page.get_by_label('Approved by a moderator', exact=True).check()
    page.get_by_role('button', name='Add these members', exact=True).click()
    expect(page.get_by_role('heading', name='What happened', exact=True)).to_be_visible()
    expect(page.locator('table')).to_contain_text('chromium-one@example.org')
    expect(page.locator('table')).to_contain_text('Subscribed')
    expect(page.locator('table')).to_contain_text('not an email address')
    page.screenshot(path=str(out / '33-mass-subscribe.png'), full_page=True)
    page.get_by_role('link', name='List members', exact=True).click()
    page.get_by_label('Search member email', exact=True).fill('chromium-one')
    page.get_by_role('button', name='Search members', exact=True).click()
    expect(page.locator('article')).to_have_count(1)
    page.get_by_role('link', name='Options', exact=True).click()
    expect(page.get_by_role('heading', name='chromium-one@example.org', exact=True)).to_be_visible()
    axe_scan(page, '/web/lists/public.example.com/members/<member>')
    page.get_by_label('Delivery mode', exact=True).select_option('mime_digests')
    page.get_by_label('Display name', exact=True).fill('Chromium One <renamed>')
    page.get_by_role('button', name='Save options', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Saved')
    expect(page.get_by_label('Delivery mode', exact=True)).to_have_value('mime_digests')
    expect(page.get_by_label('Display name', exact=True)).to_have_value('Chromium One <renamed>')
    page.get_by_role('button', name='Reset bounce score and enable delivery', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('bounce score was reset')
    page.screenshot(path=str(out / '34-member-options.png'), full_page=True)
    export = context.request.get(base + '/web/lists/public.example.com/members/export.csv')
    assert export.status == 200 and export.headers['content-type'].startswith('text/csv'), export.status
    assert 'chromium-one@example.org,Chromium One <renamed>,member' in export.text(), export.text()[:400]
    page.get_by_role('button', name='Remove from the list', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Removed 1')
    page.get_by_label('Addresses to remove', exact=True).fill('chromium-two@example.org')
    page.get_by_role('button', name='Remove selected', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('Removed 1')
    page.get_by_label('Search member email', exact=True).fill('chromium')
    page.get_by_role('button', name='Search members', exact=True).click()
    expect(page.locator('article')).to_have_count(0)
    print('Member rosters/mass subscribe/options/bounce reset/export/removal: PASS', flush=True)
    page.goto(base + '/web/account')
    public_subscription = page.locator('section').filter(has=page.get_by_role('heading', name='public.example.com', exact=True))
    public_subscription.get_by_label('Delivery status').select_option('by_user')
    public_subscription.get_by_role('button', name='Save preferences').click()
    expect(page.locator('main')).to_contain_text('Status: by_user')
    page.screenshot(path=str(out / '02-preferences-saved.png'), full_page=True)
    private_subscription = page.locator('section').filter(has=page.get_by_role('heading', name='private.example.com', exact=True))
    private_subscription.get_by_role('link', name='Read archive', exact=True).click()
    expect(page.get_by_role('heading', name='Archive: private.example.com')).to_be_visible()
    expect(page.locator('article')).to_have_count(1)
    expect(page.locator('article')).to_contain_text('Private archived body')
    page.get_by_role('link', name='Permanent link').click()
    expect(page).to_have_url(base + '/web/lists/private.example.com/archive?message=private-browser-archive')
    with page.expect_download() as attachment_download:
        page.get_by_role('link', name='Download attachment: private.csv', exact=True).click()
    attachment = attachment_download.value
    assert attachment.suggested_filename == 'attachment-0.bin'
    attachment_path = out / 'private-attachment.bin'
    attachment.save_as(str(attachment_path))
    assert attachment_path.read_bytes() == b'\xe9\x00\xff'
    with page.expect_download() as private_download:
        page.get_by_role('link', name='Download this selection (mbox)').click()
    private_path = out / 'private-archive-selection.mbox'
    private_download.value.save_as(str(private_path))
    private_box = mailbox.mbox(str(private_path), create=False)
    try:
        private_messages = list(private_box)
        assert len(private_messages) == 1
        private_payload = next(part.get_payload(decode=True) for part in private_messages[0].walk() if part.get_content_type() == 'text/plain' and part.get_content_disposition() != 'attachment')
        assert isinstance(private_payload, bytes)
        assert b'Private archived body' in private_payload
    finally:
        private_box.close()
    page.screenshot(path=str(out / '10-private-archive.png'), full_page=True)
    page.goto(base + '/web/lists/public.example.com')
    page.get_by_label('Email', exact=True).fill('browser-joined@example.com')
    page.get_by_role('button', name='Send confirmation instructions').click()
    expect(page.get_by_role('heading', name='Check your email')).to_be_visible()
    page.screenshot(path=str(out / '03-request-sent.png'), full_page=True)
    token_file = Path(os.environ['WEBUI_CONFIRMATION_FILE'])
    deadline = time.monotonic() + 15
    while not token_file.exists():
        if time.monotonic() > deadline:
            raise AssertionError('durable confirmation notice not available')
        time.sleep(.05)
    token = token_file.read_text()
    page.goto(base + '/web/lists/public.example.com/confirm')
    page.get_by_label('Token from email').fill(token)
    page.get_by_role('button', name='Confirm request', exact=True).click()
    expect(page.get_by_role('heading', name='Request confirmed', exact=True)).to_be_visible()
    page.screenshot(path=str(out / '04-request-confirmed.png'), full_page=True)
    page.goto(base + '/web/moderation')
    expect(page.locator('main')).to_contain_text('3 held, 0 requests')
    expect(page.locator('main')).to_contain_text('0 held, 1 requests')
    page.get_by_role('link', name='<script>alert(1)</script> — held messages').first.click()
    # The fixture owner can see both lists; select the held public queue explicitly.
    page.goto(base + '/web/lists/public.example.com/held')
    expect(page.get_by_role('heading', name='<script>held</script>', exact=True).first).to_be_visible()
    expect(page.locator('article[data-held]')).to_have_count(3)
    axe_scan(page, '/web/lists/public.example.com/held')
    first = page.locator('article[data-held]').first
    expect(first.locator('pre.preview')).to_contain_text('Untrusted <b>body</b>')
    first.get_by_text('Message source (first 64 KiB)', exact=True).click()
    page.screenshot(path=str(out / '05-held-escaped.png'), full_page=True)
    # The sender's posting policy and the shortcuts, from the first post.
    first.get_by_label('Posting policy for this sender').select_option('hold')
    first.get_by_role('button', name='Set', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text("sender's posting policy was set")
    first = page.locator('article[data-held]').first
    expect(first.locator('section.sender')).to_contain_text('nonmember, posting policy hold')
    # Bulk: the two later posts discarded at once.
    posts = page.locator('article[data-held]')
    posts.nth(1).get_by_label('Select', exact=True).check()
    posts.nth(2).get_by_label('Select', exact=True).check()
    page.locator('#bulk-action').select_option('discard')
    page.get_by_role('button', name='Apply to selected', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('2 decided, 0 skipped')
    expect(page.locator('article[data-held]')).to_have_count(1)
    # Keyboard: j focuses the post, a chooses accept, s submits.
    page.evaluate("document.activeElement && document.activeElement.blur()")
    page.keyboard.press('?')
    expect(page.locator('#shortcuts-help')).to_have_attribute('open', '')
    page.keyboard.press('j')
    page.keyboard.press('a')
    first = page.locator('article[data-held]').first
    expect(first.get_by_label('Decision')).to_have_value('accept')
    first.get_by_label('Comment').fill('Reviewed in Chromium: plus + and Unicode ✓')
    page.evaluate("document.activeElement && document.activeElement.blur()")
    page.keyboard.press('s')
    expect(page.locator('main')).to_contain_text('No messages await review.')
    expect(page.get_by_role('status')).to_contain_text('1 decided')
    page.screenshot(path=str(out / '06-held-accepted.png'), full_page=True)
    # The requests queue: a moderated request accepted.
    page.goto(base + '/web/lists/private.example.com/requests')
    expect(page.get_by_role('heading', name='pending-request@example.org', exact=True)).to_be_visible()
    axe_scan(page, '/web/lists/private.example.com/requests')
    page.locator('select[name="decision"]').select_option('accept')
    page.get_by_role('button', name='Apply decision', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('decision was recorded')
    expect(page.locator('main')).to_contain_text('No requests wait.')
    print('Held queue preview/sender policy/bulk/keyboard/requests: PASS', flush=True)
    # P4-LIST-CREATE-INDEX: the server owner creates a list from the
    # administration index, a taken id is refused inline, and the directory
    # finds the new list by search and domain with the reader's role shown.
    page.goto(base + '/web/admin')
    page.get_by_role('link', name='Create a list', exact=True).click()
    expect(page.get_by_role('heading', name='Create a list', exact=True)).to_be_visible()
    axe_scan(page, '/web/lists/new')
    expect(page.get_by_label('Owner address', exact=True)).to_have_value('browser@example.com')
    page.get_by_label('List name', exact=True).fill('browser-made')
    page.get_by_label('Domain', exact=True).select_option('example.com')
    page.get_by_label('Display name', exact=True).fill('Browser made')
    page.get_by_label('Style', exact=True).select_option('legacy-announce')
    page.get_by_label('Show in the public directory', exact=True).select_option('true')
    page.get_by_label('Description', exact=True).fill('Made in Chromium <b>')
    page.screenshot(path=str(out / '19-create-list.png'), full_page=True)
    page.get_by_role('button', name='Create the list', exact=True).click()
    expect(page).to_have_url(base + '/web/lists/browser-made.example.com/settings')
    expect(page.get_by_role('heading', name='List settings', exact=True)).to_be_visible()
    page.goto(base + '/web/lists/new')
    page.get_by_label('List name', exact=True).fill('browser-made')
    page.get_by_label('Domain', exact=True).select_option('example.com')
    expect_refusal()
    page.get_by_role('button', name='Create the list', exact=True).click()
    expect(page.locator('#list_name-error')).to_contain_text('already exists')
    page.goto(base + '/web?q=browser-made&domain=example.com')
    expect(page.get_by_role('link', name='Browser made', exact=True)).to_be_visible()
    expect(page.locator('main li')).to_have_count(1)
    expect(page.locator('main li').first).to_contain_text('Owner')
    expect(page.locator('main li').first).to_contain_text('Made in Chromium <b>')
    assert page.locator('script').count() == 0
    page.screenshot(path=str(out / '20-directory-filtered.png'), full_page=True)
    page.goto(base + '/web?q=browser-made&domain=other.example')
    expect(page.locator('main')).to_contain_text('No lists match.')
    page.goto(base + '/web?show=all')
    expect(page.locator('main')).to_contain_text('private.example.com')
    page.goto(base + '/web?q=browser-made')
    page.get_by_role('link', name='Browser made', exact=True).click()
    expect(page.get_by_role('heading', name='Browser made', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('browser-made@example.com')
    expect(page.locator('main')).to_contain_text('Your role')
    expect(page.locator('main')).to_contain_text('Owner')
    expect(page.get_by_role('link', name='List settings', exact=True)).to_be_visible()
    axe_scan(page, '/web/lists/browser-made.example.com')
    page.screenshot(path=str(out / '21-list-summary.png'), full_page=True)
    print('List creation/directory filters/summary: PASS', flush=True)
    # P4-DOMAINS-USERS: the server owner adds a domain, seats and removes an
    # owner, is refused a deletion while the host is mistyped, deletes the
    # empty domain, then finds an account and marks its address unverified.
    page.goto(base + '/web/admin')
    page.get_by_role('link', name='Domains', exact=True).click()
    expect(page.get_by_role('heading', name='Domains', exact=True)).to_be_visible()
    axe_scan(page, '/web/admin/domains')
    page.get_by_label('Mail host', exact=True).fill('lists.example.org')
    page.get_by_label('Description', exact=True).fill('Made in Chromium')
    page.get_by_role('button', name='Add the domain', exact=True).click()
    expect(page.get_by_role('heading', name='lists.example.org', exact=True)).to_be_visible()
    expect(page.get_by_role('status')).to_contain_text('The domain was added')
    axe_scan(page, '/web/admin/domains/lists.example.org')
    page.get_by_label('Add an owner by address', exact=True).fill('browser@example.com')
    page.get_by_role('button', name='Add owner', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('The owner was added')
    expect(page.locator('main')).to_contain_text('browser@example.com')
    page.screenshot(path=str(out / '22-domain.png'), full_page=True)
    page.get_by_role('button', name='Remove', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('The owner was removed')
    page.locator('#confirm').fill('lists.example.com')
    expect_refusal()
    page.get_by_role('button', name='Delete the domain', exact=True).click()
    expect(page.get_by_role('alert')).to_contain_text('does not match')
    page.locator('#confirm').fill('lists.example.org')
    page.get_by_role('button', name='Delete the domain', exact=True).click()
    expect(page.get_by_role('heading', name='Domains', exact=True)).to_be_visible()
    expect(page.get_by_role('status')).to_contain_text('The domain was deleted')
    expect(page.locator('main table')).not_to_contain_text('lists.example.org')
    page.get_by_role('link', name='Accounts', exact=True).click()
    expect(page.get_by_role('heading', name='Accounts', exact=True)).to_be_visible()
    page.get_by_label('Search accounts by name or address', exact=True).fill('newcomer')
    page.get_by_role('button', name='Search', exact=True).click()
    expect(page.locator('main')).to_contain_text('No accounts match.')
    page.get_by_label('Search accounts by name or address', exact=True).fill('browser@')
    page.get_by_role('button', name='Search', exact=True).click()
    axe_scan(page, '/web/admin/users')
    page.get_by_role('link', name='browser@example.com', exact=True).click()
    expect(page.get_by_role('heading', name='browser@example.com', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('public.example.com')
    page.get_by_label('Display name', exact=True).fill('Browser Owner')
    page.get_by_role('button', name='Save account', exact=True).click()
    expect(page.get_by_role('status')).to_contain_text('The account was saved')
    expect(page.get_by_role('heading', name='Browser Owner', exact=True)).to_be_visible()
    page.get_by_label('Display name', exact=True).fill('browser@example.com')
    page.get_by_role('button', name='Save account', exact=True).click()
    page.screenshot(path=str(out / '23-account-admin.png'), full_page=True)
    print('Domains/owners/deletion/accounts search and edit: PASS', flush=True)
    # P4-SYSTEM: versions, the redacted configuration, the queues, the MTA
    # map status and the audit log with a filter.
    page.goto(base + '/web/admin')
    page.get_by_role('link', name='System', exact=True).click()
    expect(page.get_by_role('heading', name='System', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('[REDACTED]')
    expect(page.locator('main')).to_contain_text('No incoming MTA is configured')
    expect(page.locator('main table').first).to_contain_text('pipeline')
    axe_scan(page, '/web/admin/system')
    page.screenshot(path=str(out / '24-system.png'), full_page=True)
    page.get_by_role('link', name='Audit log', exact=True).click()
    expect(page.get_by_role('heading', name='Audit log', exact=True)).to_be_visible()
    page.get_by_label('Action', exact=True).fill('list.create')
    page.get_by_role('button', name='Filter', exact=True).click()
    expect(page.locator('main table')).to_contain_text('browser-made.example.com')
    expect(page.locator('main table')).not_to_contain_text('member.create')
    axe_scan(page, '/web/admin/system/audit')
    page.screenshot(path=str(out / '25-audit.png'), full_page=True)
    print('System page and audit log: PASS', flush=True)
    page.goto(base + '/web/account')
    private_subscription = page.locator('section').filter(has=page.get_by_role('heading', name='private.example.com', exact=True))
    private_subscription.get_by_role('link', name='Leave list', exact=True).click()
    expect(page.get_by_role('heading', name='Leave list', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('private.example.com')
    page.screenshot(path=str(out / '12-leave-confirmation.png'), full_page=True)
    page.get_by_role('button', name='Leave this list', exact=True).click()
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    expect(page.get_by_role('heading', name='private.example.com', exact=True)).to_have_count(0)
    expect(page.get_by_role('heading', name='public.example.com', exact=True)).to_be_visible()
    departed_archive = context.request.get(base + '/web/lists/private.example.com/archive?message=private-browser-archive')
    assert departed_archive.status == 403
    assert 'Private archived body' not in departed_archive.text()
    page.get_by_role('link', name='Change password', exact=True).click()
    page.get_by_label('Current password', exact=True).fill(os.environ['WEBUI_TEST_PASSWORD'])
    page.get_by_label('New password', exact=True).fill('new strong password phrase 2026!')
    page.get_by_label('Confirm new password', exact=True).fill('new strong password phrase 2026!')
    page.get_by_role('button', name='Change password', exact=True).click()
    expect(page.get_by_role('heading', name='Password changed', exact=True)).to_be_visible()
    assert not any(c['name'] == 'listmngr_session' for c in context.cookies())
    page.screenshot(path=str(out / '11-password-changed.png'), full_page=True)
    page.get_by_role('link', name='Log in with your new password', exact=True).click()
    page.get_by_label('Email', exact=True).fill('browser@example.com')
    page.get_by_label('Password', exact=True).fill('new strong password phrase 2026!')
    page.get_by_role('button', name='Log in', exact=True).click()
    # P4-TOTP: the password alone is half a login; the next step's code is
    # accepted as drift, since confirmation consumed the current step.
    second_step(page, totp_code(totp_secret, 1))
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    # P4-ACCOUNT-PROFILE: the reader's interface language wins over the browser's.
    page.get_by_role('link', name='Your profile', exact=True).click()
    expect(page.get_by_role('heading', name='Your profile', exact=True)).to_be_visible()
    page.get_by_label('Display name', exact=True).fill('Trình duyệt <b>')
    page.get_by_label('Interface language', exact=True).select_option('vi')
    page.get_by_label('Time zone', exact=True).select_option('Asia/Ho_Chi_Minh')
    page.get_by_role('button', name='Save profile', exact=True).click()
    expect(page.get_by_role('heading', name='Hồ sơ của bạn', exact=True)).to_be_visible()
    assert page.evaluate('document.documentElement.lang') == 'vi', 'profile language applied'
    expect(page.get_by_label('Múi giờ', exact=True)).to_have_value('Asia/Ho_Chi_Minh')
    page.screenshot(path=str(out / '13-profile-vietnamese.png'), full_page=True)
    page.get_by_label('Ngôn ngữ giao diện', exact=True).select_option('en')
    page.get_by_label('Tên hiển thị', exact=True).fill('Browser Reader')
    page.get_by_role('button', name='Lưu hồ sơ', exact=True).click()
    expect(page.get_by_role('heading', name='Your profile', exact=True)).to_be_visible()
    assert page.evaluate('document.documentElement.lang') == 'en'
    page.goto(base + '/web/account')
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    # P4-ACCOUNT-ADDRESSES: the account's addresses, with a new one added unverified.
    page.get_by_role('link', name='Your email addresses', exact=True).click()
    expect(page.get_by_role('heading', name='Your email addresses', exact=True)).to_be_visible()
    expect(page.get_by_role('heading', name='browser@example.com', exact=True)).to_be_visible()
    page.get_by_label('New address', exact=True).fill('browser-second@example.com')
    page.get_by_role('button', name='Add address', exact=True).click()
    expect(page.get_by_role('heading', name='browser-second@example.com', exact=True)).to_be_visible()
    expect(page.locator('main')).to_contain_text('Unverified')
    page.screenshot(path=str(out / '18-addresses.png'), full_page=True)
    page.goto(base + '/web/account')
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    # P4-ACCOUNT-TOKENS: the (server-owner) reader mints a token bound to one
    # list, sees the secret once, and revokes it.
    page.get_by_role('link', name='API tokens', exact=True).click()
    expect(page.get_by_role('heading', name='API tokens', exact=True)).to_be_visible()
    assert page.locator('input[name="scopes"][value="admin"]').count() == 1, 'a server owner may mint administrative scopes'
    page.get_by_label('Name', exact=True).fill('browser token')
    page.get_by_label('members:read', exact=True).check()
    page.get_by_label('List', exact=True).select_option('public.example.com')
    page.get_by_role('button', name='Create token', exact=True).click()
    expect(page.get_by_text('Copy this token now')).to_be_visible()
    issued = page.locator('code').inner_text()
    assert issued.startswith('lm_'), 'the secret is shown once'
    page.get_by_role('link', name='API tokens', exact=True).click()
    expect(page.get_by_role('heading', name='browser token', exact=True)).to_be_visible()
    assert issued not in page.content(), 'the secret is not shown again'
    page.screenshot(path=str(out / '19-tokens.png'), full_page=True)
    page.get_by_role('button', name='Revoke token', exact=True).click()
    expect(page.locator('main')).to_contain_text('Revoked')
    page.goto(base + '/web/account')
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    # P4-ACCOUNT-DELETE: the confirmation page asks for the password; the
    # server-owner fixture account is the last server owner, so it is refused
    # by the HTTP tests and not attempted here.
    page.get_by_role('link', name='Delete account', exact=True).click()
    expect(page.get_by_role('heading', name='Delete your account', exact=True)).to_be_visible()
    expect(page.get_by_label('Your password', exact=True)).to_be_visible()
    page.screenshot(path=str(out / '20-delete-account.png'), full_page=True)
    page.goto(base + '/web/account')
    # P4-ACCOUNT-SESSIONS: the reader sees this browser's own session and can end it.
    page.get_by_role('link', name='Signed-in browsers', exact=True).click()
    expect(page.get_by_role('heading', name='Signed-in browsers', exact=True)).to_be_visible()
    expect(page.get_by_role('heading', name='This browser', exact=True)).to_be_visible()
    page.screenshot(path=str(out / '12-sessions.png'), full_page=True)
    page.get_by_role('button', name='Sign this browser out', exact=True).click()
    expect(page.get_by_role('heading', name='Mailing lists')).to_be_visible()
    assert not any(c['name'] == 'listmngr_session' for c in context.cookies()), 'ending this session cleared its cookie'
    denied_account = context.request.get(base + '/web/account')
    assert denied_account.status == 401
    page.goto(base + '/web/login')
    page.get_by_label('Email', exact=True).fill('browser@example.com')
    page.get_by_label('Password', exact=True).fill('new strong password phrase 2026!')
    page.get_by_role('button', name='Log in', exact=True).click()
    # P4-TOTP: a recovery code completes the login once.
    second_step(page, recovery_codes[0])
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    page.get_by_role('button', name='Log out', exact=True).click()
    expect(page.get_by_role('heading', name='Mailing lists')).to_be_visible()
    assert not any(c['name'] == 'listmngr_session' for c in context.cookies())
    # P4-WEBAUTHN: a passwordless login with the passkey, then its removal.
    page.goto(base + '/web/login')
    page.get_by_role('button', name='Sign in with a passkey', exact=True).click()
    expect(page.get_by_role('heading', name='My subscriptions', exact=True)).to_be_visible()
    page.goto(base + '/web/admin')
    expect(page.get_by_role('heading', name='List administration', exact=True)).to_be_visible()
    page.goto(base + '/web/account/passkeys')
    page.get_by_label('Your password', exact=True).fill('new strong password phrase 2026!')
    page.get_by_role('button', name='Remove passkey', exact=True).click()
    expect(page.locator('main')).to_contain_text('You have no passkeys.')
    cdp.send('WebAuthn.removeVirtualAuthenticator', {'authenticatorId': virtual})
    page.goto(base + '/web/account')
    page.get_by_role('button', name='Log out', exact=True).click()
    expect(page.get_by_role('heading', name='Mailing lists')).to_be_visible()
    assert not any(c['name'] == 'listmngr_session' for c in context.cookies())
    denied_archive = context.request.get(base + '/web/lists/private.example.com/archive?message=private-browser-archive&format=mbox')
    assert denied_archive.status == 403
    assert 'Private archived body' not in denied_archive.text()
    denied_attachment = context.request.get(base + '/web/lists/private.example.com/archive?message=private-browser-archive&attachment=0')
    assert denied_attachment.status == 403
    assert denied_attachment.body() != b'\xe9\x00\xff'
    page.set_viewport_size({'width': 390, 'height': 844})
    page.screenshot(path=str(out / '07-mobile-directory.png'), full_page=True)
    assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'no horizontal overflow'
    page.goto(base + '/web/lists/public.example.com')
    page.get_by_role('link', name='Browse public archive').click()
    expect(page.get_by_role('heading', name='Archive: public.example.com')).to_be_visible()
    expect(page.locator('article')).to_have_count(1)
    expect(page.locator('article')).to_contain_text('Archived text <script>unsafe</script>')
    assert page.locator('script').count() == 0
    page.get_by_label('Search archive').fill('absent')
    page.get_by_role('button', name='Search', exact=True).click()
    expect(page.locator('main')).to_contain_text('No messages found.')
    page.get_by_label('Search archive').fill('Archived text')
    page.get_by_role('button', name='Search', exact=True).click()
    expect(page.locator('article')).to_have_count(1)
    page.get_by_role('link', name='View thread').click()
    expect(page.locator('article')).to_have_count(1)
    assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'archive mobile overflow'
    page.screenshot(path=str(out / '08-mobile-archive.png'), full_page=True)
    page.get_by_role('link', name='Permanent link').click()
    expect(page).to_have_url(base + '/web/lists/public.example.com/archive?message=browser-archive')
    expect(page.locator('article')).to_have_count(1)
    expect(page.locator('article')).to_contain_text('Archived text <script>unsafe</script>')
    page.reload()
    expect(page.locator('article')).to_have_count(1)
    assert page.locator('script').count() == 0
    page.screenshot(path=str(out / '09-message-permalink.png'), full_page=True)
    with page.expect_download() as download_info:
        page.get_by_role('link', name='Download this selection (mbox)').click()
    download = download_info.value
    assert download.suggested_filename == 'archive.mbox'
    downloaded_path = out / 'archive-selection.mbox'
    download.save_as(str(downloaded_path))
    exported = mailbox.mbox(str(downloaded_path), create=False)
    try:
        messages = list(exported)
        assert len(messages) == 1, 'permalink export contains exactly one message'
        assert str(messages[0]['Subject']).endswith('Browser archive fixture')
        payload = messages[0].get_payload(decode=True)
        assert isinstance(payload, bytes)
        assert b'Archived text <script>unsafe</script>' in payload
    finally:
        exported.close()
    # P4-SHELL: one template shell — document language, current-page marking,
    # both colour schemes, and no third-party asset on any page.
    page.goto(base + '/web')
    assert page.evaluate('document.documentElement.lang') == 'en', 'shell language'
    assert page.locator('nav a[aria-current="page"]').count() == 1, 'the current page is marked'
    light = page.evaluate('getComputedStyle(document.documentElement).backgroundColor')
    dark_context = browser.new_context(color_scheme='dark')
    dark_page = dark_context.new_page()
    dark_page.goto(base + '/web')
    dark = dark_page.evaluate('getComputedStyle(document.documentElement).backgroundColor')
    assert dark != light, f'dark scheme is not distinct: {dark}'
    dark_page.screenshot(path=str(out / '14-dark-directory.png'), full_page=True)
    dark_context.close()
    vietnamese = browser.new_context(locale='vi-VN')
    vietnamese_page = vietnamese.new_page()
    vietnamese_page.on('console', lambda msg: errors.append(msg.text) if msg.type == 'error' else None)
    vietnamese_page.goto(base + '/web')
    assert vietnamese_page.evaluate('document.documentElement.lang') == 'vi', 'negotiated language'
    expect(vietnamese_page.get_by_role('link', name='Đăng ký của tôi', exact=True)).to_be_visible()
    vietnamese_page.goto(base + '/web/lists/public.example.com')
    expect(vietnamese_page.get_by_role('button', name='Gửi hướng dẫn xác nhận', exact=True)).to_be_visible()
    vietnamese_page.screenshot(path=str(out / '15-vietnamese-list.png'), full_page=True)
    vietnamese.close()
    # P4-ACCOUNT-SIGNUP: an anonymous visitor creates an account; the page never
    # says whether the address had one, and the token arrives by mail.
    page.goto(base + '/web/login')
    page.get_by_role('link', name='Create an account', exact=True).click()
    expect(page.get_by_role('heading', name='Create an account', exact=True)).to_be_visible()
    page.get_by_label('Email', exact=True).fill('newcomer@example.com')
    page.get_by_label('Display name', exact=True).fill('Newcomer <b>')
    page.get_by_label('Password', exact=True).fill('walrus-corridor-lantern-92')
    page.get_by_label('Confirm password', exact=True).fill('walrus-corridor-lantern-92')
    page.get_by_role('button', name='Create account', exact=True).click()
    expect(page.get_by_role('heading', name='Check your email', exact=True)).to_be_visible()
    page.screenshot(path=str(out / '16-signup-accepted.png'), full_page=True)
    # The verification page prefills a token from the link; a bad token is
    # refused with a 400, which the HTTP tests cover (a deliberate 400 would
    # count as a console error here).
    page.goto(base + '/web/verify?token=not-a-real-token')
    expect(page.get_by_role('heading', name='Verify your email address', exact=True)).to_be_visible()
    expect(page.get_by_label('Token from email', exact=True)).to_have_value('not-a-real-token')
    # P4-ACCOUNT-RESET: the reset request looks the same for any address; the
    # confirmation page prefills the token from the link.
    page.goto(base + '/web/login')
    page.get_by_role('link', name='Forgot your password?', exact=True).click()
    expect(page.get_by_role('heading', name='Reset your password', exact=True)).to_be_visible()
    page.get_by_label('Email', exact=True).fill('browser@example.com')
    page.get_by_role('button', name='Send reset instructions', exact=True).click()
    expect(page.get_by_role('heading', name='Check your email', exact=True)).to_be_visible()
    page.screenshot(path=str(out / '17-reset-requested.png'), full_page=True)
    page.goto(base + '/web/reset/confirm?token=not-a-real-token')
    expect(page.get_by_role('heading', name='Choose a new password', exact=True)).to_be_visible()
    expect(page.get_by_label('Token from email', exact=True)).to_have_value('not-a-real-token')
    headers = context.request.get(base + '/web').headers
    assert headers['content-security-policy'] == (
        "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; "
        "frame-ancestors 'none'"
    ), headers['content-security-policy']
    htmx = context.request.get(base + '/web/htmx.min.js')
    assert htmx.status == 200 and len(htmx.body()) > 10000, 'htmx is served from this origin'

    # Accessibility of the public pages; the signed-in settings pages were
    # scanned in place above.
    if axe_source is not None:
        for path in [
            '/web',
            '/web/login',
            '/web/lists/public.example.com',
            '/web/lists/public.example.com/archive',
        ]:
            page.goto(base + path)
            axe_scan(page, path)
        print(f'AXE PASS: no critical or serious violations on {len(scanned)} pages.', flush=True)
    else:
        print('AXE SKIPPED: set WEBUI_AXE_SCRIPT to a local axe.min.js to scan.', flush=True)
    assert not errors, errors
    print(f'CHROMIUM PASS ({browser.version}): rendered CSS; login; saved preference; public request/confirm; escaped held source; accept; logout; public archive/search/thread; mobile layout; second factor enrolled from the shown secret, two-step login with a drifted app code and once with a recovery code; a passkey registered through a virtual authenticator, used for a passwordless login and removed; profile edited with the interface language switching to Vietnamese and back; a second address added unverified; a bound API token minted, shown once and revoked; the delete-account confirmation reached; own session listed and ended; anonymous signup accepted and the verification page prefilled; a reset requested for a verified account; shell language/current-page/dark scheme/Vietnamese negotiation; CSP header and origin-served htmx; zero console/page errors. Screenshots contain no credentials. Settings groups previewed/refused inline/saved, a header rule added, tested and removed, a ban added and lifted, a template previewed, saved and removed, and the delete confirmation refused a wrong id. Member rosters per role, an htmx search swap, mass subscription with outcomes, a member\'s options and bounce reset, a CSV export, and removals by button and by pasted address. Held queue with a rendered preview, the sender moderated from the post, two posts discarded in bulk, one accepted by keyboard, and a subscription request accepted. A list created from the administration index, a taken id refused inline, the directory searched and filtered by domain with the owner badge, and the new list\'s summary with its addresses. A domain added, an owner seated and removed, a mistyped deletion refused, the empty domain deleted, an account searched, opened and renamed. The system page with redacted configuration and queues, and the audit log filtered by action.')
    context.close()
    browser.close()
