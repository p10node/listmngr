//! Vietnamese bodies for the template catalog. Names not listed here fall
//! back to English in [`super::templates::builtin_in`].

/// The Vietnamese body for `name`, when translated.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "domain:admin:notice:new-list" => {
            "Hộp thư chung '$listname' vừa được tạo cho bạn.  Dưới đây là một số thông\n\
             tin cơ bản về hộp thư chung của bạn.\n\
             \n\
             Có một giao diện qua email dành cho người dùng (không phải quản trị viên)\n\
             của hộp thư; để biết cách dùng, hãy gửi một thư chỉ có từ 'help' ở tiêu\n\
             đề hoặc nội dung tới:\n\
             \n    $request_email\n\
             \n\
             Mọi thắc mắc xin gửi tới $site_email.\n"
        }
        "list:admin:action:post" => {
            "Với tư cách quản trị viên hộp thư chung, bạn được yêu cầu duyệt bài gửi\n\
             sau:\n\
             \n    Hộp thư:  $listname\n    Từ:       $sender_email\n    Tiêu đề:  $subject\n\
             \n\
             Bài đang được giữ lại vì:\n\
             \n$reasons\n\
             \n\
             Khi thuận tiện, hãy vào bảng điều khiển để chấp nhận hoặc từ chối.\n"
        }
        "list:admin:action:subscribe" => {
            "Cần bạn phê duyệt một yêu cầu đăng ký hộp thư chung:\n\
             \n    Cho:      $member\n    Hộp thư:  $listname\n"
        }
        "list:admin:action:unsubscribe" => {
            "Cần bạn phê duyệt một yêu cầu rời hộp thư chung:\n\
             \n    Cho:      $member\n    Hộp thư:  $listname\n"
        }
        "list:admin:notice:disable" => {
            "Đăng ký của $member trên $listname đã bị tạm ngưng vì điểm thư dội vượt\n\
             ngưỡng bounce_score_threshold của hộp thư.\n"
        }
        "list:admin:notice:increment" => {
            "Điểm thư dội của $member trên $listname đã tăng lên $score.\n"
        }
        "list:admin:notice:pending" => {
            "Hộp thư $listname có $count yêu cầu đang chờ điều hành.\n\
             \n$data\n\
             \n\
             Vui lòng xử lý khi thuận tiện sớm nhất.\n"
        }
        "list:admin:notice:removal" => "$member đã bị gỡ khỏi $listname vì thư dội quá nhiều.\n",
        "list:admin:notice:subscribe" => "$member đã đăng ký thành công vào $display_name.\n",
        "list:admin:notice:unrecognized" => {
            "Thư đính kèm được nhận như một thư dội, nhưng hoặc định dạng thư dội không\n\
             nhận diện được, hoặc không trích được địa chỉ thành viên nào từ đó.  Hộp\n\
             thư này được cấu hình để chuyển mọi thư dội không nhận diện được tới quản\n\
             trị viên.\n"
        }
        "list:admin:notice:unsubscribe" => "$member đã bị gỡ khỏi $display_name.\n",
        "list:member:digest:masthead" => {
            "Gửi bài cho hộp thư chung $display_name tới\n\
             \t$listname\n\
             \n\
             Để đăng ký hoặc rời hộp thư qua email, gửi thư có tiêu đề hoặc nội dung\n\
             'help' tới\n\
             \t$request_email\n\
             \n\
             Bạn có thể liên hệ người quản lý hộp thư tại\n\
             \t$owner_email\n\
             \n\
             Khi trả lời, hãy sửa tiêu đề cho cụ thể hơn \"Re: Nội dung bản tổng hợp\n\
             $display_name...\"\n"
        }
        "list:member:generic:footer" | "list:member:regular:footer" => {
            "_______________________________________________\n\
             Hộp thư chung $display_name -- $listname\n\
             Để rời hộp thư, gửi email tới ${short_listname}-leave@${domain}\n"
        }
        "list:user:action:invite" => {
            "Địa chỉ \"$user_email\" của bạn được mời tham gia hộp thư chung $listname\n\
             tại $domain bởi chủ hộp thư $listname.  Bạn có thể nhận lời mời bằng cách\n\
             trả lời thư này và giữ nguyên tiêu đề.\n\
             \n\
             Hoặc gửi một thư tới $request_email chỉ chứa đúng dòng sau:\n\
             \n    confirm $token\n\
             \n\
             Với hầu hết trình đọc thư, chỉ cần bấm 'trả lời' là được.\n\
             \n\
             Nếu bạn muốn từ chối lời mời, chỉ cần bỏ qua thư này.  Nếu có thắc mắc,\n\
             hãy gửi tới $owner_email.\n"
        }
        "list:user:action:subscribe" => {
            "Xác nhận đăng ký địa chỉ email\n\
             \n\
             Xin chào, đây là máy chủ hộp thư chung tại $domain.\n\
             \n\
             Chúng tôi nhận được yêu cầu đăng ký cho địa chỉ email\n\
             \n    $user_email\n\
             \n\
             Trước khi dùng hộp thư chung $listname, bạn cần xác nhận đây đúng là địa\n\
             chỉ email của bạn.  Hãy trả lời thư này và giữ nguyên tiêu đề.\n\
             \n\
             Bạn cũng có thể xác nhận qua HTTPS bằng cách gửi\n\
             POST $confirm_uri với nội dung JSON {\"token\":\"$token\"}.\n\
             Token: $token\n\
             Mã này hết hạn sau 24 giờ và chỉ dùng được một lần.\n\
             \n\
             Nếu bạn không muốn đăng ký địa chỉ này, chỉ cần bỏ qua thư.  Nếu nghĩ\n\
             mình bị người khác cố ý đăng ký, hoặc có thắc mắc, hãy liên hệ\n\
             \n    $owner_email\n"
        }
        "list:user:action:unsubscribe" => {
            "Xác nhận rời hộp thư chung\n\
             \n\
             Xin chào, đây là máy chủ hộp thư chung tại $domain.\n\
             \n\
             Chúng tôi nhận được yêu cầu rời hộp thư cho địa chỉ email\n\
             \n    $user_email\n\
             \n\
             Trước khi được gỡ khỏi hộp thư chung $listname, bạn cần xác nhận đây\n\
             đúng là địa chỉ email của bạn.  Hãy trả lời thư này và giữ nguyên tiêu đề.\n\
             \n\
             Bạn cũng có thể xác nhận qua HTTPS bằng cách gửi\n\
             POST $confirm_uri với nội dung JSON {\"token\":\"$token\"}.\n\
             Token: $token\n\
             Mã này hết hạn sau 24 giờ và chỉ dùng được một lần.\n\
             \n\
             Nếu bạn không muốn rời hộp thư, chỉ cần bỏ qua thư này.  Nếu có thắc mắc,\n\
             hãy liên hệ\n\
             \n    $owner_email\n"
        }
        "list:user:notice:goodbye" => {
            "Bạn đã rời khỏi hộp thư chung \"$display_name\" ($listname).\n\
             \n\
             Nếu có thắc mắc, bạn có thể liên hệ\n\
             \n    $owner_email\n"
        }
        "list:user:notice:autoresponse" => {
            "Đây là thư trả lời tự động từ hộp thư chung $listname tại $domain.\n\
             Thư của bạn đã được nhận. Chưa ai đọc nó; thư sẽ được xử lý sau. Nếu cần\n\
             người trả lời ngay, hãy viết tới $owner_email.\n\
             \n\
             Địa chỉ này sẽ không trả lời tự động cho bạn thêm lần nữa trong một\n\
             thời gian.\n"
        }
        "list:user:notice:echo" => {
            "Đây là bot lệnh của $listname tại $domain trả lời lệnh `echo` của bạn\n\
             cùng đoạn văn bản mà nó mang theo:\n\
             \n    $echo\n\
             \n\
             Không có gì thay đổi. Gửi `help` tới $request_email để xem các lệnh hộp\n\
             thư chung này hiểu.\n"
        }
        "list:user:notice:help" => {
            "Gửi một lệnh ở tiêu đề, hoặc ở dòng text/plain đầu tiên không trống của\n\
             nội dung khi tiêu đề để trống, tới $request_email.\n\
             Khi dùng Trả lời, hãy thay tiêu đề bằng đúng một lệnh (ví dụ: join). Đừng\n\
             giữ tiêu đề của thư hướng dẫn này.\n\
             Để liên hệ người quản trị, hãy viết riêng tới $owner_email; trả lời thư\n\
             này sẽ tới bot lệnh.\n\
             \n\
             join hoặc subscribe: xin tham gia bằng địa chỉ gửi thư của bạn.\n\
             leave hoặc unsubscribe: xin rời đi bằng địa chỉ gửi thư của bạn.\n\
             confirm TOKEN: xác nhận mã một lần đã gửi tới địa chỉ đó.\n\
             help: hướng dẫn giới hạn này, tối đa một lần mỗi địa chỉ/hộp thư/giờ.\n\
             echo TEXT: trả lại đúng đoạn văn bản đó, cùng hạn mức như help.\n\
             end hoặc stop: dừng đọc lệnh (ví dụ trước chữ ký).\n\
             Không hỗ trợ tham số địa chỉ, mật khẩu, lệnh điều hành hay chuỗi nhiều\n\
             lệnh.\n"
        }
        "list:user:notice:hold" => {
            "Thư của bạn gửi tới '$listname' với tiêu đề\n\
             \n    $subject\n\
             \n\
             đang được giữ lại cho tới khi người điều hành xem xét và duyệt.\n\
             \n\
             Thư bị giữ lại vì:\n\
             \n$reasons\n\
             \n\
             Thư sẽ được đăng lên hộp thư, hoặc bạn sẽ nhận được thông báo về quyết\n\
             định của người điều hành.\n"
        }
        "list:user:notice:no-more-today" => {
            "Chúng tôi nhận được thư từ địa chỉ <$sender_email> của bạn yêu cầu phản\n\
             hồi tự động từ hộp thư chung $listname.\n\
             \n\
             Số lần đã thấy hôm nay: $count.  Để tránh vòng lặp thư giữa các robot\n\
             email, chúng tôi sẽ không gửi thêm phản hồi nào cho bạn hôm nay.  Vui\n\
             lòng thử lại vào ngày mai.\n\
             \n\
             Nếu bạn cho rằng thư này là nhầm lẫn, hoặc có thắc mắc, hãy liên hệ chủ\n\
             hộp thư tại $owner_email.\n"
        }
        "list:user:notice:post" => {
            "Thư của bạn với tiêu đề\n\
             \n    $subject\n\
             \n\
             đã được hộp thư chung $display_name nhận thành công.\n"
        }
        "list:user:notice:probe" => {
            "Đây là thư thăm dò.  Bạn có thể bỏ qua thư này.\n\
             \n\
             Hộp thư chung $listname đã nhận nhiều thư dội từ bạn, cho thấy có thể có\n\
             vấn đề khi gửi thư tới $sender_email.  Hãy kiểm tra để chắc rằng địa chỉ\n\
             email của bạn không gặp sự cố.  Bạn có thể hỏi người quản trị hệ thống\n\
             thư của mình để được trợ giúp.\n\
             \n\
             Bạn không cần làm gì để tiếp tục là thành viên đang nhận thư.\n\
             \n\
             Nếu có thắc mắc hoặc sự cố, bạn có thể liên hệ chủ hộp thư tại\n\
             \n    $owner_email\n"
        }
        "list:user:notice:receipt" => {
            "$outcome $listname.\n\
             Biên nhận này ghi lại kết quả tại thời điểm xác nhận.\n\
             Để được trợ giúp, gửi email tới $request_email với tiêu đề help.\n\
             Để liên hệ người quản trị, hãy viết tới $owner_email.\n"
        }
        "list:user:notice:refuse" => {
            "Yêu cầu của bạn gửi tới hộp thư chung $listname\n\
             \n    $request\n\
             \n\
             đã bị người điều hành từ chối.  Lý do người điều hành đưa ra:\n\
             \n\"$reason\"\n\
             \n\
             Mọi thắc mắc hoặc góp ý xin gửi tới quản trị viên hộp thư tại:\n\
             \n    $owner_email\n"
        }
        "list:user:notice:rejected" => {
            "Thư của bạn gửi tới hộp thư chung $listname đã bị từ chối vì các lý do\n\
             sau:\n\
             \n$reasons\n"
        }
        "list:user:notice:warning" => {
            "Đăng ký của bạn tại hộp thư chung $listname đã bị tạm ngưng vì thư dội\n\
             quá nhiều.  Bạn sẽ không nhận thêm thư nào từ hộp thư này cho tới khi bật\n\
             lại đăng ký.\n\
             \n\
             Để bật lại, hãy vào trang tùy chọn của bạn hoặc liên hệ chủ hộp thư.\n\
             \n\
             Nếu có thắc mắc hoặc sự cố, bạn có thể liên hệ chủ hộp thư tại\n\
             \n    $owner_email\n"
        }
        "list:user:notice:welcome" => {
            "Chào mừng bạn đến với hộp thư chung \"$display_name\"!\n\
             \n\
             Để gửi bài lên hộp thư, hãy gửi email tới:\n\
             \n  $listname\n\
             \n\
             Bạn có thể rời hộp thư hoặc chỉnh tùy chọn của mình qua email bằng cách\n\
             gửi thư tới:\n\
             \n  $request_email\n\
             \n\
             với từ 'help' ở tiêu đề hoặc nội dung (không kèm dấu nháy), bạn sẽ nhận\n\
             lại thư hướng dẫn.\n"
        }
        _ => return None,
    })
}
