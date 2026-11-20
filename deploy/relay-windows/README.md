# Hướng dẫn cài Proxy Relay riêng trên máy chủ Windows 10 (`10.7.12.233`)

`litechat-relay.exe` là proxy siêu nhẹ viết bằng Rust (không cần Python/Node), hỗ trợ **cả SOCKS5h (phân giải DNS từ xa trên máy chủ) lẫn HTTP CONNECT** trên cùng một cổng `1088`, có xác thực tài khoản/mật khẩu và mặc định chỉ cho phép đi qua các tên miền cần thiết cho LiteChat (tùy chọn `-AllowAll` nếu muốn mở tất cả).

## 1. Sao chép file sang máy chủ `10.7.12.233` qua SSH/SCP
Từ máy dev, chạy:
```powershell
scp J:\litechat\target\release\litechat-relay.exe J:\litechat\deploy\relay-windows\install-relay.ps1 <USER_WINDOWS>@10.7.12.233:C:/Temp/relay/
```
*(Nhớ tạo sẵn thư mục `C:\Temp\relay` trên máy đích trước hoặc `ssh <USER>@10.7.12.233 "mkdir C:\Temp\relay"`.)*

## 2. Cài đặt làm dịch vụ chạy ngầm trên `10.7.12.233`
Chạy từ PowerShell (Administrator) trên `10.7.12.233` hoặc trực tiếp qua SSH (nếu user là Administrator):
```powershell
powershell -ExecutionPolicy Bypass -File C:\Temp\relay\install-relay.ps1 -Port 1088 -Username litechat
```
- Script tự sinh mật khẩu ngẫu nhiên an toàn (hoặc truyền `-Password "..."` theo ý bạn), khóa quyền đọc cấu hình ở `C:\ProgramData\LiteChatRelay\config.json` chỉ cho `SYSTEM` + `Administrators`, mở Firewall cổng `1088`, và đăng ký `LiteChatRelay` tự khởi động cùng Windows dưới quyền `SYSTEM`.
- Bạn có thể giới hạn chỉ cho phép dải mạng nội bộ kết nối bằng `-AllowedSubnet "10.7.0.0/16"`.

## 3. Cấu hình trên LiteChat ở các máy nội bộ không có Internet
Mở **Cài đặt LiteChat** → **Mạng nội bộ / Relay**:
- Bật **Sử dụng mạng Relay**
- Giao thức: **SOCKS5h (Khuyên dùng)** hoặc **HTTP CONNECT**
- Máy chủ: `10.7.12.233`, Cổng: `1088`
- Tài khoản & Mật khẩu: lấy từ kết quả của `install-relay.ps1`
- Nhấn **Kiểm tra kết nối Relay** → **Lưu cấu hình Relay**.

## 4. Gỡ cài đặt
```powershell
powershell -ExecutionPolicy Bypass -File .\uninstall-relay.ps1
```
