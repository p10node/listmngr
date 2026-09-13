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
