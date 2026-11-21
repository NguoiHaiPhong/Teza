# Kiểm kê Tuân thủ Bản quyền & Bảo mật (`Teza`)

Tài liệu này là checklist bắt buộc cho mọi giai đoạn phát triển của **Teza** (khởi nguồn từ `litechat`).

## 1. Quy tắc Bản quyền & Giấy phép
1. **Không sao chép mã nguồn GPLv3:** Tuyệt đối không copy mã nguồn từ `tdesktop` hay bất kỳ dự án GPLv3 nào. Chỉ tham khảo nguyên lý UX/UI hiện đại (cột thư mục, bảng phím tắt, mật độ hiển thị) và tự viết mới hoàn toàn bằng TypeScript/React + Tailwind.
2. **Không phân phối tài sản độc quyền bên thứ ba:**
   - Không đưa thư mục `web-bundle/`, APK/XAPK, `.so`, `.dex`, hay file `.wasm` độc quyền vào repo.
   - Nếu sau này cần tích hợp thành phần runtime bổ trợ từ bên thứ ba, chỉ kiểm tra tại máy người dùng ở runtime, không đóng gói kèm bộ cài.
3. **Không nhúng tài sản đồ họa chưa rõ bản quyền:**
   - Không nhúng sẵn bộ sticker hay tài sản đồ họa có bản quyền vào mã nguồn/binary.
   - Icon UI chỉ dùng các bộ icon mã nguồn mở giấy phép thoáng (`lucide-react`, `MIT`/`ISC`).
4. **Nhận diện thương hiệu riêng:** Tên sản phẩm chính thức là **Teza** (tên kỹ thuật monorepo/crate lịch sử là `litechat` / `zalo-core`), không sử dụng tên, logo hoặc biểu tượng của các dịch vụ nhắn tin bên thứ ba trên giao diện.

## 2. Quy tắc Bảo mật & Khắc phục nợ kỹ thuật
| Hạng mục | Thực trạng cần tránh | Chuẩn bắt buộc trong `Teza` |
| :--- | :--- | :--- |
| **Lưu trữ dữ liệu Windows** | Lưu vào `cwd` (`Program Files` bị lỗi quyền ghi) | Luôn dùng `%LOCALAPPDATA%\Teza\` (tự di trú từ `%LOCALAPPDATA%\litechat\` nếu có), trừ khi chỉ định `--data-dir`. |
| **Khóa phiên & API Key (`zpw_sek`, `zpw_enk`, khóa dịch)** | Lưu plaintext 100% trong JSON | Mã hóa khóa nhạy cảm bằng **Windows DPAPI** (`CryptProtectData`) trước khi ghi đĩa; ghi atomic + không log bí mật. |
| **Sinh số ngẫu nhiên (`ParamCipher v2`)** | Dùng PRNG yếu seed bằng timestamp/PID | Bắt buộc dùng CSPRNG của hệ điều hành (`getrandom`). |
| **Local API / HTTP Server** | Mở HTTP port `127.0.0.1` với `CORS *` và token tùy chọn | Dùng Tauri IPC nội bộ có kiểm tra capability; **không** mở HTTP server không xác thực. |
| **Thông báo & Dialog Windows** | Spawn `powershell.exe` cho Toast và Folder Picker | Dùng cửa sổ Toast Tauri/Win32 trực tiếp & Windows Shell Dialog qua Tauri plugin/native API. |
| **Auto-updater Windows** | Nhầm lẫn `portable` vs `setup`, bỏ sót `.exe.old` | Dùng Tauri Updater có chữ ký Ed25519 / SHA-256 và tách biệt rõ kênh cài đặt. |
