# Notice subjects. Bodies live in the template catalog (listmngr-mail).
notice-welcome-subject = Welcome to the "{ $display_name }" mailing list
notice-goodbye-subject = You have been unsubscribed from the { $display_name } mailing list
notice-autoresponse-subject = Auto-response for your message to the "{ $display_name }" mailing list
notice-probe-subject = Bounce probe from the { $listname } mailing list
notice-echo-subject = List command echo
notice-help-subject = List email command help
notice-receipt-subject = List { $action } request completed
notice-rejected-subject = Request to mailing list "{ $display_name }" rejected
notice-hold-subject = Your message to { $listname } awaits moderator approval
notice-admin-post-subject = { $listname } post from { $sender } requires approval
notice-bounce-disable-subject = { $member }'s subscription disabled on { $listname }
notice-bounce-increment-subject = { $member }'s bounce score incremented on { $listname }
notice-bounce-removal-subject = { $member } unsubscribed from { $listname } mailing list due to bounces
notice-warning-subject = Your subscription for { $listname } mailing list has been disabled
notice-unknown-sender = (unknown sender)
notice-no-subject = (no subject)
receipt-join-outcome = Your join request has completed. You are subscribed to
receipt-leave-outcome = Your leave request has completed. You are not subscribed to

# Literal in every language: the reply-to-confirm parser depends on it.
confirm-subject = confirm { $token }

# Content filter (`filter_action = forward`).
notice-content-filter-subject = Content filter message notification
content-filter-forward-body =
    The attached message matched the { $display_name } mailing list's content
    filtering rules and was prevented from being forwarded on to the list
    membership.  You are receiving the only remaining copy of the discarded
    message.

# Mailman's `acknowledge` handler.
notice-post-ack-subject = { $display_name } post acknowledgment

# Mailman's `notify`: the daily reminder of what moderators still owe.
notice-pending-subject = The { $listname } list has { $count } moderation requests waiting.
notify-held-messages = Held messages:
notify-held-subscriptions = Held subscriptions:
notify-held-unsubscriptions = Held unsubscriptions:
notify-more = ... and { $count } more

# Mailman's `admin_notify_mchanges`: owners and moderators learn of
# membership changes.
notice-admin-subscribe-subject = { $display_name } subscription notification
notice-admin-unsubscribe-subject = { $display_name } unsubscription notification

# Mailman's `forward` on a moderator decision.
notice-forward-subject = Forward of moderated message
forward-moderated-body = A moderator of { $display_name } forwarded the attached held message to you.

# Browser interface (P4-SHELL). Titles double as link text where a page links
# to another page; a value is never assembled from fragments at runtime.
web-skip-to-content = Skip to content
web-nav-label = Main
web-nav-lists = Lists
web-nav-account = My subscriptions
web-nav-moderation = Moderation
web-nav-login = Log in
web-footer = Mailing lists, managed by your community.
web-pagination-label = Pagination
web-pagination-previous = Previous page
web-pagination-next = Next page
web-title-directory = Mailing lists
web-title-login = Log in
web-title-account = My subscriptions
web-title-admin = List administration
web-title-members = List members
web-title-settings = List settings
web-title-password = Change password
web-title-password-changed = Password changed
web-title-leave = Leave list
web-title-recover = Restore delivery
web-title-check-email = Check your email
web-title-confirm = Confirm your request
web-title-confirmed = Request confirmed
web-title-moderation = Moderator queues
web-title-held = Held messages
web-title-error = Request not completed
web-title-archive = Archive: { $list }
web-title-unsubscribe = Unsubscribe
web-title-unsubscribed = Unsubscribed
web-error-body = The request was invalid, expired or not authorized. No action was completed.
web-error-login-again = Log in again
web-error-return = or return to the list and try again.
web-login-email = Email
web-login-password = Password
web-login-submit = Log in
web-login-no-signup = Account signup and password reset are not available in this interface. Contact the site administrator.
web-account-signed-in = Signed in as { $name }.
web-account-logout = Log out
web-account-delivery = Delivery: { $mode }. Status: { $status }.
web-account-read-archive = Read archive
web-account-restore-delivery = Restore delivery
web-account-delivery-restricted = Delivery is restricted. Contact a list administrator.
web-account-delivery-mode = Delivery mode
web-account-delivery-status = Delivery status
web-account-own-postings = Receive your own posts
web-account-list-copy = Receive list copies when directly addressed
web-account-save-preferences = Save preferences
web-account-leave = Leave list
web-delivery-regular = Individual messages
web-delivery-plaintext = Plain text digest
web-delivery-mime = MIME digest
web-status-enabled = Enabled
web-status-paused = Paused by me
web-yes = Yes
web-no = No
web-admin-scope = Only lists you own or administer are shown.
web-members-intro = Members of { $list }. Overrides affect future posting decisions, not already held or queued mail. Other safety checks still apply.
web-members-search-label = Search member email
web-members-search-submit = Search members
web-members-clear-search = Clear search
web-members-none = No matching members.
web-members-policy-label = Posting policy
web-members-save-policy = Save posting policy
web-policy-default = Use list default
web-policy-defer = Defer (currently accepts after safety checks)
web-policy-accept = Accept
web-policy-hold = Hold for review
web-policy-reject = Reject
web-policy-discard = Discard
web-settings-intro = Settings for { $list }. Posting defaults apply to future decisions; safety checks and member overrides still apply. System fallback uses the server's configured posting action. Archive policy controls access and future archiving, not deletion of stored mail.
web-settings-display-name = Display name
web-settings-description = Description
web-settings-subject-prefix = Subject prefix
web-settings-subject-prefix-help = Leave empty for no prefix. Spaces and Unicode are preserved; line breaks are not allowed. Applies when future outgoing posts are composed, not to mail already sent.
web-settings-emergency = Emergency moderation
web-settings-advertised = Show in public directory
web-settings-welcome = Send welcome messages
web-settings-goodbye = Send goodbye messages
web-settings-member-action = Default member posting action
web-settings-nonmember-action = Default nonmember posting action
web-settings-archive-policy = Archive policy
web-settings-emergency-help = Emergency moderation holds otherwise eligible new posts for review, even when posting defaults accept them. It is not a delivery shutdown: already queued mail and explicit moderator approvals can still be delivered. Turning it off does not release held posts.
web-settings-max-size = Maximum message size (KiB)
web-settings-max-size-help = Hold original posts larger than this size, including headers and attachments. 1 KiB is 1024 bytes; 0 disables this per-list limit, not the server intake limit.
web-settings-max-recipients = To/Cc recipient hold threshold
web-settings-max-recipients-help = Hold posts at or above this visible To/Cc mailbox count. Repeated addresses count; Bcc and list subscribers do not. Unparseable headers are held when enabled. 0 disables this check.
web-settings-notice-help = Welcome and goodbye messages apply to future completed subscriptions and removals. Changing these settings does not send notices to existing subscribers or recall queued notices.
web-settings-save = Save list settings
web-action-default = Use system fallback
web-action-defer = Defer (accept after safety checks)
web-archive-public = Public
web-archive-private = Private
web-archive-never = Never
web-password-current = Current password
web-password-new = New password
web-password-confirm = Confirm new password
web-password-signs-out = Changing your password signs you out on all browsers.
web-password-submit = Change password
web-password-changed-body = All your browser sessions have been signed out.
web-password-changed-login = Log in with your new password
web-leave-prompt = Remove the membership for { $email } on { $list }? This removes only this subscription, not your account or any owner/moderator role. Already queued mail may still arrive.
web-leave-submit = Leave this list
web-cancel = Cancel
web-recover-prompt = Verify that your mailbox is working before restoring delivery for { $email } on { $list }. This resets this subscription's bounce score and warning cycle. No mailbox challenge or probe is sent.
web-recover-submit = Restore delivery
web-list-browse-archive = Browse public archive
web-list-email = Email
web-list-request = Request
web-list-join = Join
web-list-leave = Leave
web-list-submit = Send confirmation instructions
web-list-confirmation-note = Mailbox confirmation is required. Requests are limited to one per list and address per hour. Mail delivery must be enabled by the administrator.
web-list-enter-token = Enter a confirmation token from your email
web-check-email-body = If eligible, confirmation instructions will be sent. Copy the Token from the message into the list’s confirmation form. No membership change has been made yet.
web-confirm-intro = Confirm a join or leave request for { $list }. Opening this page does not change your subscription.
web-confirm-token = Token from email
web-confirm-submit = Confirm request
web-confirmed-body = Your subscription request has been completed.
web-moderation-held = { $name } — held messages
web-moderation-scope = Only lists you are authorized to moderate are shown.
web-held-none = No messages await review.
web-held-sender = Sender
web-held-reason = Reason
web-held-source = Message source (first 64 KiB)
web-held-decision = Decision
web-held-defer = Keep held
web-held-accept = Accept for delivery
web-held-reject = Reject
web-held-discard = Discard
web-held-comment = Comment
web-held-apply = Apply decision
web-held-note = Accept queues regular delivery using the current recipient preferences. Reject and discard record the decision without sending a rejection notice.
web-archive-search = Search archive
web-archive-search-submit = Search
web-archive-all-threads = All threads
web-archive-download = Download this selection (mbox)
web-archive-download-note = Downloads contain at most 20 messages, not a complete archive backup.
web-archive-none = No messages found.
web-archive-view-thread = View thread
web-archive-permalink = Permanent link
web-archive-attachment = Download attachment: { $name }
web-archive-attachments-unavailable = Attachments unavailable: MIME invalid or exceeds attachment limits.
web-archive-previous = Previous
web-archive-next = Next
web-unsubscribe-prompt = Confirm that you want to leave the { $list } mailing list ({ $address }).
web-unsubscribe-submit = Unsubscribe
web-unsubscribed-prompt = You have been unsubscribed from the { $list } mailing list ({ $address }). No further mail from this list will be sent to you.
web-title-sessions = Signed-in browsers
web-sessions-intro = Every browser currently signed in to your account. Ending a session signs that browser out immediately; it does not change your password.
web-sessions-this-browser = This browser
web-sessions-other-browser = Another browser
web-sessions-started = Signed in
web-sessions-expires = Expires
web-sessions-end = End this session
web-sessions-end-this = Sign this browser out
web-sessions-end-others = End every other session
web-sessions-note = Sessions also end on their own at the time shown, and changing your password ends all of them. This page shows browser sessions only, not API tokens.
web-account-sessions-link = Signed-in browsers
web-title-profile = Your profile
web-account-profile-link = Your profile
web-profile-intro = Your name as pages and notices show it, the language this interface uses for you, and your time zone.
web-profile-display-name = Display name
web-profile-locale = Interface language
web-profile-timezone = Time zone
web-profile-note = The interface language applies to pages you open while signed in and takes precedence over your browser's language preference. The language of list notices is a subscription preference and is not changed here.
web-profile-save = Save profile
# Language names are endonyms in every catalog, as a language picker shows them.
web-language-en = English
web-language-vi = Tiếng Việt

# Mail the site sends outside any list (P4-SITE-NOTICES).
notice-site-verify-subject = Confirm your email address for { $site_name }
notice-site-reset-subject = Reset your password for { $site_name }
