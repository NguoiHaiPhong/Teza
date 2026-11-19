# Kiểm kê Tuân thủ Bản quyền & Bảo mật (`litechat`)

Tài liệu này là checklist bắt buộc cho mọi Phase phát triển (Task T0.3).

## 1. Quy tắc Bản quyền & Giấy phép
1. **Không sao chép mã nguồn GPLv3:** Tuyệt đối không copy mã nguồn từ `tdesktop` (Telegram Desktop) hay bất kỳ dự án GPLv3 nào. Chỉ học hỏi bố cục UX/UI (cột thư mục, bảng phím tắt, mật độ hiển thị) và tự viết mới bằng TypeScript/React + Tailwind.
2. **Không phân phối tài sản độc quyền của Zalo/VNG:**
   - Không đưa thư mục `web-bundle/`, APK/XAPK, `.so`, `.dex`, hay file `.wasm` của Zalo vào repo.
   - Nếu sau này hỗ trợ Sync v2 qua `libzproto_wasm`, chỉ tìm ở runtime trên máy người dùng (nếu có sẵn Zalo PC), không đóng gói kèm bộ cài.
3. **Không nhúng tài sản đồ họa bên thứ ba chưa rõ bản quyền:**
   - Không nhúng sẵn bộ sticker `ami_fat_cat` hay asset có bản quyền vào mã nguồn/binary.
   - Icon UI chỉ dùng các bộ icon mã nguồn mở giấy phép thoáng (ví dụ `lucide-react`, `MIT`/`ISC`).
4. **Nhận diện thương hiệu riêng:** Tên ứng dụng là `litechat`, không sử dụng tên, logo hoặc biểu tượng của Zalo hay Telegram.

## 2. Quy tắc Bảo mật & Khắc phục nợ kỹ thuật từ `ctdozlite`
| Hạng mục | Thực trạng cần tránh | Chuẩn bắt buộc trong `litechat` |
| :--- | :--- | :--- |
| **Lưu trữ dữ liệu Windows** | Lưu vào `cwd` (`Program Files` bị lỗi quyền ghi) | Luôn dùng `%LOCALAPPDATA%\litechat\` (qua `dirs` / `SHGetKnownFolderPath`), trừ khi chỉ định `--data-dir`. |
| **Khóa phiên (`zpw_sek`, `zpw_enk`)** | Lưu plaintext 100% trong `session.json` | Mã hóa khóa nhạy cảm bằng **Windows DPAPI** (`CryptProtectData`) trước khi ghi đĩa; ghi atomic + không log bí mật. |
| **Sinh số ngẫu nhiên (`ParamCipher v2`)** | Dùng `xorshift64*` seed bằng timestamp/PID | Bắt buộc dùng CSPRNG của hệ điều hành (`getrandom`). |
| **Local API / HTTP Server** | Mở HTTP port `127.0.0.1` với `CORS *` và token tùy chọn | MVP dùng Tauri IPC nội bộ có kiểm tra capability; **không** mở HTTP server không xác thực. |
| **Thông báo & Dialog Windows** | Spawn `powershell.exe` cho Toast và Folder Picker | Dùng Windows Toast / WinRT & Win32 Shell Dialog trực tiếp (hoặc Tauri native plugin). |
| **Auto-updater Windows** | Nhầm lẫn `portable` vs `setup`, bỏ sót `.exe.old` | Dùng Tauri Updater có chữ ký Ed25519 / SHA-256 và tách biệt rõ kênh cài đặt. |
