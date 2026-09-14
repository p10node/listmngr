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

# Mailman's `forward` on a moderator decision.
notice-forward-subject = Chuyển tiếp thư đang chờ duyệt
forward-moderated-body = Một người duyệt của { $display_name } đã chuyển tiếp cho bạn thư đang chờ duyệt đính kèm.

# Giao diện trình duyệt (P4-SHELL).
web-skip-to-content = Bỏ qua, đến nội dung chính
web-nav-label = Điều hướng chính
web-nav-lists = Danh sách thư
web-nav-account = Đăng ký của tôi
web-nav-moderation = Kiểm duyệt
web-nav-login = Đăng nhập
web-footer = Hộp thư chung, do cộng đồng của bạn quản lý.
web-pagination-label = Phân trang
web-pagination-previous = Trang trước
web-pagination-next = Trang sau
web-title-directory = Danh sách thư
web-title-login = Đăng nhập
web-title-account = Đăng ký của tôi
web-title-admin = Quản trị hộp thư chung
web-title-members = Thành viên hộp thư chung
web-title-settings = Cài đặt hộp thư chung
web-title-password = Đổi mật khẩu
web-title-password-changed = Đã đổi mật khẩu
web-title-leave = Rời hộp thư chung
web-title-recover = Khôi phục nhận thư
web-title-check-email = Hãy kiểm tra hộp thư của bạn
web-title-confirm = Xác nhận yêu cầu của bạn
web-title-confirmed = Yêu cầu đã được xác nhận
web-title-moderation = Hàng chờ kiểm duyệt
web-title-held = Thư đang bị giữ
web-title-error = Yêu cầu chưa hoàn tất
web-title-archive = Kho lưu trữ: { $list }
web-title-unsubscribe = Hủy đăng ký
web-title-unsubscribed = Đã hủy đăng ký
web-error-body = Yêu cầu không hợp lệ, đã hết hạn hoặc không được phép. Không có thao tác nào được thực hiện.
web-error-login-again = Đăng nhập lại
web-error-return = hoặc quay lại hộp thư chung và thử lần nữa.
web-login-email = Địa chỉ email
web-login-password = Mật khẩu
web-login-submit = Đăng nhập
web-login-no-signup = Giao diện này chưa hỗ trợ tạo tài khoản và đặt lại mật khẩu. Hãy liên hệ quản trị viên của máy chủ.
web-account-signed-in = Đang đăng nhập với tên { $name }.
web-account-logout = Đăng xuất
web-account-delivery = Cách nhận: { $mode }. Trạng thái: { $status }.
web-account-read-archive = Đọc kho lưu trữ
web-account-restore-delivery = Khôi phục nhận thư
web-account-delivery-restricted = Việc nhận thư đang bị hạn chế. Hãy liên hệ quản trị viên của hộp thư chung.
web-account-delivery-mode = Cách nhận thư
web-account-delivery-status = Trạng thái nhận thư
web-account-own-postings = Nhận lại bài của chính mình
web-account-list-copy = Nhận bản sao từ hộp thư chung khi đã được gửi trực tiếp
web-account-save-preferences = Lưu tùy chọn
web-account-leave = Rời hộp thư chung
web-delivery-regular = Từng thư riêng lẻ
web-delivery-plaintext = Bản tổng hợp văn bản thuần
web-delivery-mime = Bản tổng hợp MIME
web-status-enabled = Đang bật
web-status-paused = Tôi đang tạm dừng
web-yes = Có
web-no = Không
web-admin-scope = Chỉ hiển thị những hộp thư chung bạn sở hữu hoặc quản trị.
web-members-intro = Thành viên của { $list }. Thiết lập riêng chỉ ảnh hưởng tới các quyết định gửi bài về sau, không ảnh hưởng thư đang bị giữ hay đang chờ trong hàng đợi. Các kiểm tra an toàn khác vẫn được áp dụng.
web-members-search-label = Tìm theo email thành viên
web-members-search-submit = Tìm thành viên
web-members-clear-search = Xóa điều kiện tìm
web-members-none = Không có thành viên nào khớp.
web-members-policy-label = Chính sách gửi bài
web-members-save-policy = Lưu chính sách gửi bài
web-policy-default = Dùng mặc định của hộp thư chung
web-policy-defer = Hoãn quyết định (hiện chấp nhận sau các kiểm tra an toàn)
web-policy-accept = Chấp nhận
web-policy-hold = Giữ lại để duyệt
web-policy-reject = Từ chối
web-policy-discard = Loại bỏ
web-settings-intro = Cài đặt của { $list }. Mặc định gửi bài áp dụng cho các quyết định về sau; các kiểm tra an toàn và thiết lập riêng của thành viên vẫn có hiệu lực. Mặc định hệ thống dùng hành động gửi bài đã cấu hình cho máy chủ. Chính sách lưu trữ quyết định quyền truy cập và việc lưu trữ về sau, không xóa thư đã lưu.
web-settings-display-name = Tên hiển thị
web-settings-description = Mô tả
web-settings-subject-prefix = Tiền tố tiêu đề
web-settings-subject-prefix-help = Để trống nếu không muốn có tiền tố. Khoảng trắng và Unicode được giữ nguyên; không cho phép xuống dòng. Chỉ áp dụng khi soạn các bài gửi đi về sau, không áp dụng cho thư đã gửi.
web-settings-emergency = Kiểm duyệt khẩn cấp
web-settings-advertised = Hiện trong danh bạ công khai
web-settings-welcome = Gửi thư chào mừng
web-settings-goodbye = Gửi thư tạm biệt
web-settings-member-action = Hành động mặc định cho bài của thành viên
web-settings-nonmember-action = Hành động mặc định cho bài của người ngoài
web-settings-archive-policy = Chính sách lưu trữ
web-settings-emergency-help = Kiểm duyệt khẩn cấp giữ lại để duyệt mọi bài mới lẽ ra đủ điều kiện, kể cả khi mặc định gửi bài chấp nhận chúng. Đây không phải là việc ngắt gửi thư: thư đã vào hàng đợi và các phê duyệt tường minh của kiểm duyệt viên vẫn có thể được gửi đi. Tắt tùy chọn này cũng không giải phóng các bài đang bị giữ.
web-settings-max-size = Kích thước thư tối đa (KiB)
web-settings-max-size-help = Giữ lại các bài gốc lớn hơn kích thước này, tính cả phần đầu thư và tệp đính kèm. 1 KiB là 1024 byte; giá trị 0 tắt giới hạn riêng của hộp thư chung, không tắt giới hạn tiếp nhận của máy chủ.
web-settings-max-recipients = Ngưỡng giữ thư theo số người nhận To/Cc
web-settings-max-recipients-help = Giữ lại các bài có số hộp thư hiện trong To/Cc bằng hoặc vượt ngưỡng này. Địa chỉ lặp lại vẫn được tính; Bcc và người đăng ký của hộp thư chung thì không. Khi bật, thư có phần đầu không phân tích được cũng bị giữ. Giá trị 0 tắt kiểm tra này.
web-settings-notice-help = Thư chào mừng và thư tạm biệt áp dụng cho các lượt đăng ký và rời đi hoàn tất về sau. Thay đổi các cài đặt này không gửi thông báo cho người đã đăng ký, cũng không thu hồi thông báo đang chờ trong hàng đợi.
web-settings-save = Lưu cài đặt hộp thư chung
web-action-default = Dùng mặc định của hệ thống
web-action-defer = Hoãn quyết định (chấp nhận sau các kiểm tra an toàn)
web-archive-public = Công khai
web-archive-private = Riêng tư
web-archive-never = Không bao giờ
web-password-current = Mật khẩu hiện tại
web-password-new = Mật khẩu mới
web-password-confirm = Nhập lại mật khẩu mới
web-password-signs-out = Đổi mật khẩu sẽ đăng xuất bạn trên mọi trình duyệt.
web-password-submit = Đổi mật khẩu
web-password-changed-body = Mọi phiên trình duyệt của bạn đã được đăng xuất.
web-password-changed-login = Đăng nhập bằng mật khẩu mới
web-leave-prompt = Gỡ tư cách thành viên của { $email } tại { $list }? Thao tác này chỉ gỡ đăng ký này, không gỡ tài khoản của bạn cũng như vai trò sở hữu/kiểm duyệt. Thư đã vào hàng đợi vẫn có thể được gửi tới.
web-leave-submit = Rời hộp thư chung này
web-cancel = Hủy bỏ
web-recover-prompt = Hãy kiểm tra hộp thư của bạn hoạt động bình thường trước khi khôi phục việc nhận thư cho { $email } tại { $list }. Thao tác này đặt lại điểm thư trả về và chu kỳ cảnh báo của đăng ký này. Không có thư thử hay thư dò nào được gửi đi.
web-recover-submit = Khôi phục nhận thư
web-list-browse-archive = Xem kho lưu trữ công khai
web-list-email = Địa chỉ email
web-list-request = Yêu cầu
web-list-join = Tham gia
web-list-leave = Rời đi
web-list-submit = Gửi hướng dẫn xác nhận
web-list-confirmation-note = Bắt buộc xác nhận từ hộp thư. Mỗi hộp thư chung và mỗi địa chỉ chỉ được một yêu cầu mỗi giờ. Quản trị viên phải bật việc gửi thư.
web-list-enter-token = Nhập mã xác nhận từ email của bạn
web-check-email-body = Nếu đủ điều kiện, hướng dẫn xác nhận sẽ được gửi đi. Hãy chép mã trong thư vào biểu mẫu xác nhận của hộp thư chung. Chưa có thay đổi nào về tư cách thành viên.
web-confirm-intro = Xác nhận yêu cầu tham gia hoặc rời khỏi { $list }. Mở trang này không thay đổi đăng ký của bạn.
web-confirm-token = Mã từ email
web-confirm-submit = Xác nhận yêu cầu
web-confirmed-body = Yêu cầu đăng ký của bạn đã hoàn tất.
web-moderation-held = { $name } — thư đang bị giữ
web-moderation-scope = Chỉ hiển thị những hộp thư chung bạn được phép kiểm duyệt.
web-held-none = Không có thư nào chờ duyệt.
web-held-sender = Người gửi
web-held-reason = Lý do
web-held-source = Nguồn thư (64 KiB đầu)
web-held-decision = Quyết định
web-held-defer = Tiếp tục giữ
web-held-accept = Chấp nhận để gửi đi
web-held-reject = Từ chối
web-held-discard = Loại bỏ
web-held-comment = Ghi chú
web-held-apply = Áp dụng quyết định
web-held-note = Chấp nhận sẽ đưa thư vào hàng đợi gửi thường theo tùy chọn hiện tại của người nhận. Từ chối và loại bỏ chỉ ghi nhận quyết định mà không gửi thông báo từ chối.
web-archive-search = Tìm trong kho lưu trữ
web-archive-search-submit = Tìm kiếm
web-archive-all-threads = Tất cả luồng
web-archive-download = Tải phần đang chọn (mbox)
web-archive-download-note = Mỗi lần tải chứa tối đa 20 thư, không phải bản sao lưu đầy đủ của kho lưu trữ.
web-archive-none = Không tìm thấy thư nào.
web-archive-view-thread = Xem luồng
web-archive-permalink = Liên kết cố định
web-archive-attachment = Tải tệp đính kèm: { $name }
web-archive-attachments-unavailable = Không xem được tệp đính kèm: MIME không hợp lệ hoặc vượt giới hạn đính kèm.
web-archive-previous = Trước
web-archive-next = Sau
web-unsubscribe-prompt = Xác nhận rằng bạn muốn rời khỏi hộp thư chung { $list } ({ $address }).
web-unsubscribe-submit = Hủy đăng ký
web-unsubscribed-prompt = Bạn đã hủy đăng ký khỏi hộp thư chung { $list } ({ $address }). Hộp thư chung này sẽ không gửi thêm thư nào cho bạn.
web-title-sessions = Trình duyệt đang đăng nhập
web-sessions-intro = Mọi trình duyệt đang đăng nhập vào tài khoản của bạn. Kết thúc một phiên sẽ đăng xuất trình duyệt đó ngay lập tức; thao tác này không đổi mật khẩu.
web-sessions-this-browser = Trình duyệt này
web-sessions-other-browser = Trình duyệt khác
web-sessions-started = Đăng nhập lúc
web-sessions-expires = Hết hạn
web-sessions-end = Kết thúc phiên này
web-sessions-end-this = Đăng xuất trình duyệt này
web-sessions-end-others = Kết thúc mọi phiên khác
web-sessions-note = Phiên cũng tự hết hạn vào thời điểm hiển thị, và đổi mật khẩu sẽ kết thúc tất cả. Trang này chỉ hiển thị phiên trình duyệt, không gồm API token.
web-account-sessions-link = Trình duyệt đang đăng nhập
