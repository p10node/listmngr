# Tiêu đề thông báo. Nội dung nằm trong catalog template (listmngr-mail).
notice-welcome-subject = Chào mừng bạn đến với hộp thư chung "{ $display_name }"
notice-goodbye-subject = Bạn đã rời khỏi hộp thư chung { $display_name }
notice-autoresponse-subject = Trả lời tự động cho thư bạn gửi tới hộp thư chung "{ $display_name }"
notice-probe-subject = Thư dò thư dội từ hộp thư chung { $listname }
notice-echo-subject = Lệnh echo của hộp thư chung
notice-help-subject = Hướng dẫn lệnh email của hộp thư chung
notice-receipt-subject = Yêu cầu { $action ->
    [join] tham gia
    [leave] rời khỏi
   *[other] { $action }
  } hộp thư chung đã hoàn tất
notice-rejected-subject = Yêu cầu gửi tới hộp thư chung "{ $display_name }" bị từ chối
notice-hold-subject = Thư của bạn gửi tới { $listname } đang chờ người điều hành duyệt
notice-admin-post-subject = Bài gửi tới { $listname } từ { $sender } cần được duyệt
notice-bounce-disable-subject = Đăng ký của { $member } trên { $listname } đã bị tạm ngưng
notice-bounce-increment-subject = Điểm thư dội của { $member } trên { $listname } đã tăng
notice-bounce-removal-subject = { $member } đã bị gỡ khỏi hộp thư chung { $listname } vì thư dội
notice-warning-subject = Đăng ký của bạn tại hộp thư chung { $listname } đã bị tạm ngưng
notice-unknown-sender = (không rõ người gửi)
notice-no-subject = (không có tiêu đề)
receipt-join-outcome = Yêu cầu tham gia của bạn đã hoàn tất. Bạn đã đăng ký vào
receipt-leave-outcome = Yêu cầu rời đi của bạn đã hoàn tất. Bạn không còn đăng ký vào

# Literal in every language: the reply-to-confirm parser depends on it.
confirm-subject = confirm { $token }

# Content filter (`filter_action = forward`).
notice-content-filter-subject = Thông báo thư bị bộ lọc nội dung chặn
content-filter-forward-body =
    Thư đính kèm khớp với quy tắc lọc nội dung của hộp thư chung { $display_name }
    nên không được chuyển tiếp tới các thành viên.  Bạn đang nhận bản sao
    duy nhất còn lại của thư đã bị loại bỏ.

# Mailman's `acknowledge` handler.
notice-post-ack-subject = Xác nhận đã nhận bài gửi tới { $display_name }

# Lệnh `notify` của Mailman: nhắc hằng ngày những gì người điều hành còn nợ.
notice-pending-subject = Hộp thư { $listname } có { $count } yêu cầu đang chờ điều hành.
notify-held-messages = Thư đang giữ:
notify-held-subscriptions = Yêu cầu đăng ký đang chờ:
notify-held-unsubscriptions = Yêu cầu rời đi đang chờ:
notify-more = ... và { $count } yêu cầu nữa

# Mailman's `admin_notify_mchanges`: owners and moderators learn of
# membership changes.
notice-admin-subscribe-subject = Thông báo đăng ký { $display_name }
notice-admin-unsubscribe-subject = Thông báo huỷ đăng ký { $display_name }
