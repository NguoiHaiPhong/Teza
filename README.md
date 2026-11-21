<div align="center">
  <img src="apps/desktop/src-tauri/icons/128x128.png" width="76" height="76" alt="Teza Logo" />
  <h1>Teza</h1>
  <p><strong>Ứng dụng nhắn tin desktop gọn nhẹ, tốc độ cao và bảo mật cho Windows</strong></p>
  <p>
    <a href="https://github.com/NguoiHaiPhong/litechat/releases/latest">Tải bản phát hành mới nhất</a> ·
    <a href="#hướng-dẫn-sử-dụng">Hướng dẫn sử dụng</a> ·
    <a href="docs/ARCHITECTURE.md">Kiến trúc hệ thống</a> ·
    <a href="docs/LEGAL_AND_SECURITY.md">Pháp lý &amp; Bảo mật</a>
  </p>
</div>

---

## Giới thiệu sản phẩm

**Teza** là ứng dụng nhắn tin desktop mã nguồn mở dành cho Windows, được xây dựng với lõi **Rust** hiệu năng cao kết hợp giao diện **Tauri v2 + React + TypeScript + Tailwind CSS**.

Thay vì phụ thuộc vào trình duyệt nặng nề, Teza tối ưu tốc độ phản hồi khi soạn thảo và chuyển hội thoại, mã hóa cục bộ dữ liệu nhạy cảm bằng **Windows DPAPI**, lưu trữ lịch sử trò chuyện bằng **SQLite FTS5** (hỗ trợ tìm kiếm không dấu), đồng thời bổ sung nhiều tiện ích làm việc chuyên sâu như thư mục phân loại hội thoại, thông báo nổi 4 góc màn hình, dịch tự động đa nhà cung cấp, nhắc hẹn, bình chọn và **Web App nhúng**.

> **Lưu ý về mã nguồn & lưu trữ:** Teza được phát triển từ nền tảng dự án `litechat` (tên repository hiện tại vẫn là `NguoiHaiPhong/litechat`). Khi chạy lần đầu, ứng dụng tự động di trú dữ liệu từ `%LOCALAPPDATA%\litechat\` (nếu có) sang `%LOCALAPPDATA%\Teza\`.

---

## Tính năng nổi bật

- **Giao diện 3 cột thích ứng & cá nhân hóa sâu:**
  - Tự động co giãn theo kích thước cửa sổ; kéo thanh phân cách để đổi độ rộng danh sách hội thoại.
  - **4 chủ đề màu:** *Obsidian Vàng Kim*, *Night Blue*, *Dark Violet*, *Light*.
  - Tùy chỉnh cỡ chữ (`Nhỏ / Vừa / Lớn`), mật độ dòng hội thoại (`Thoáng / Gọn`), hiệu ứng hạt sáng ánh kim (*Hiệu ứng Tiên Sáng*), và **3 chế độ hiển thị thanh thư mục bên trái** (*Icon + Chữ*, *Chỉ Icon*, *Chỉ Chữ* căn lề trái).
- **Đa ngôn ngữ & Dịch hội thoại:**
  - Giao diện hỗ trợ **Tiếng Việt**, **English** và **한국어 (Tiếng Hàn)**.
  - Hỗ trợ dịch từng tin nhắn hoặc toàn bộ hội thoại qua **DeepL**, **Google Gemini**, **Azure Translator**; tự động đồng bộ ngôn ngữ đích theo ngôn ngữ giao diện đang chọn.
- **Nhắn tin & Cộng tác nhóm:**
  - Trích dẫn trả lời, **Trả lời dạng chuỗi (Thread)** ở bảng phụ bên phải, thả cảm xúc với hơn **300 emoji** và bộ nhãn dán (*Sticker*).
  - Dán trực tiếp ảnh chụp màn hình hoặc tệp từ Clipboard (`Ctrl + V`), xem ảnh độ phân giải cao bằng **Media Lightbox**.
  - Ghim tin nhắn, chọn nhiều tin nhắn để chuyển tiếp/xóa, nhắc tên thành viên bằng `@`, **Thêm thành viên vào nhóm**.
  - Tạo và bỏ phiếu **Bình chọn (Poll)** trong nhóm; tạo **Nhắc hẹn (Reminder)** và hiển thị dưới dạng thẻ nhắc hẹn gọn gàng.
- **Phân loại & Tìm kiếm tốc độ cao:**
  - Thư mục mặc định (*Tất cả*, *Chưa đọc*, *Cá nhân*, *Nhóm*, *Cloud / Saved*) + **Thư mục tùy chỉnh** (tùy biến icon, sắp xếp thứ tự, ẩn/hiện thư mục, loại trừ hội thoại).
  - **Hội thoại đã lưu trữ (Archive)**, ghim hội thoại quan trọng lên đầu, tìm kiếm toàn văn không dấu bằng SQLite FTS5 (`Ctrl + K`), tìm bạn mới qua số điện thoại.
- **Thông báo nổi & Tập trung:**
  - Cửa sổ thông báo nổi độc lập có thể đặt ở **4 góc màn hình** (trên-trái, trên-phải, dưới-trái, dưới-phải); bấm vào thông báo sẽ đưa cửa sổ chính lên và mở thẳng vào cuộc hội thoại đó.
  - Bật/tắt riêng thông báo cho chat 1-1, nhóm, hoặc chỉ báo khi được nhắc tên (`@`); tùy chọn ẩn nội dung trên thông báo; tắt tiếng có thời hạn (`1 giờ / 4 giờ / 8 giờ / 24 giờ / Mãi mãi`); chạy nền qua khay hệ thống (*System Tray*).
- **Web App nhúng ngay trong Teza:**
  - Bảng quản lý Web App truy cập trực tiếp từ thanh công cụ bên trái, cho phép ghim trang web công việc (Notion, Jira, Drive, trang nội bộ...) để mở **nhúng trực tiếp trong khung chính Teza** hoặc mở bằng trình duyệt ngoài.
- **Bảo mật cục bộ, Relay Proxy & Auto-Update:**
  - Khóa phiên và API key dịch thuật được mã hóa bằng **Windows DPAPI** (`CryptProtectData`).
  - **Khóa ứng dụng bằng mã PIN** (băm cục bộ PBKDF2-SHA256, hỗ trợ tự động khóa sau 1/5/15/60 phút hoặc khóa nhanh bằng `Ctrl + L`).
  - Hỗ trợ kết nối qua **Mạng Relay** (SOCKS5 / HTTP CONNECT kèm công cụ `litechat-relay`) cho môi trường mạng hạn chế.
  - Cập nhật tự động có xác thực chữ ký **Ed25519**.

---

## Cài đặt & Cập nhật

### Yêu cầu hệ thống
- **Hệ điều hành:** Windows 10 hoặc Windows 11 (64-bit).
- **Thành phần phụ thuộc:** SQLite, TLS/Crypto và Visual C++ Runtime đã được đóng gói tĩnh trong tệp cài đặt. Nếu máy chưa có **Microsoft Edge WebView2 Runtime**, bộ cài sẽ tự động tải bootstrapper và cài đặt ngầm trong quá trình cài Teza (cần kết nối Internet).

### Tải và cài đặt
1. Truy cập trang [Releases](https://github.com/NguoiHaiPhong/litechat/releases/latest).
2. Tải về tệp cài đặt Windows mới nhất:
   - `Teza_<phiên_bản>_x64-setup.exe` *(khuyến nghị cho cài đặt cá nhân)*, hoặc
   - `Teza_<phiên_bản>_x64_en-US.msi`
3. Chạy tệp cài đặt và làm theo hướng dẫn trên màn hình.

### Kiểm tra & cài đặt bản cập nhật
Mở **Menu chính (`☰`) → Kiểm tra cập nhật**, hoặc vào **Cài đặt → Hệ thống & Phím tắt → Khay hệ thống & Cập nhật** để kiểm tra và nâng cấp phiên bản mới trực tiếp trong ứng dụng.

---

## Hướng dẫn sử dụng

### 1. Đăng nhập lần đầu
- Mở **Teza**, ứng dụng hiển thị màn hình đăng nhập bằng **mã QR**.
- Dùng ứng dụng di động tương ứng quét mã QR trên màn hình để xác thực phiên.
- Nếu máy tính đang ở trong mạng doanh nghiệp/nội bộ cần đi qua proxy, bấm nút **Mạng Relay** ngay trên màn hình QR để cấu hình trước khi đăng nhập.

### 2. Bố cục 3 cột & Menu chính (`☰`)
- **Cột 1 (Thanh thư mục ngoài cùng bên trái):** Chứa nút **Menu chính (`☰`)**, các thư mục lọc hội thoại, nút **Thư mục** (quản lý thư mục) và nút **Web App** (nếu đã bật).
- **Cột 2 (Danh sách hội thoại):** Kéo đường viền giữa Cột 2 và Cột 3 để thay đổi độ rộng theo ý muốn. Dùng ô tìm kiếm phía trên để lọc nhanh hoặc tìm theo số điện thoại.
- **Cột 3 (Khung trò chuyện chính & Bảng thông tin bên phải):** Hiển thị nội dung tin nhắn, khung soạn thảo và nút mở bảng thông tin chi tiết hội thoại ở góc trên bên phải.
- Bấm **`☰` (Menu chính)** để truy cập nhanh: *Saved Messages / Cloud cá nhân*, *Tìm bạn mới*, *Quản lý thư mục*, *Hội thoại đã lưu trữ*, *Khóa ứng dụng*, *Cài đặt*, *Dịch hội thoại*, đổi nhanh chủ đề màu, kiểm tra cập nhật và đăng xuất.

### 3. Phân loại hội thoại, Ghim, Tắt thông báo & Lưu trữ
- **Chuột phải lên một cuộc hội thoại** để mở menu thao tác:
  - **Ghim / Bỏ ghim** hội thoại lên đầu danh sách.
  - **Đánh dấu đã đọc / chưa đọc**.
  - **Tắt thông báo** trong `1 giờ`, `4 giờ`, `8 giờ`, `24 giờ` hoặc `Mãi mãi`.
  - **Gán vào thư mục tùy chỉnh** hoặc **Lưu trữ hội thoại** (đưa vào mục *Hội thoại đã lưu trữ* để gọn danh sách chính).
- Vào **Cài đặt → Thư mục hội thoại** để tạo/sắp xếp thư mục tùy chỉnh, ẩn/hiện các thư mục mặc định (*Chưa đọc*, *Cá nhân*, *Nhóm*, *Cloud*) và đổi kiểu hiển thị trên thanh bên (*Icon + Chữ*, *Chỉ Icon*, *Chỉ Chữ*).

### 4. Nhắn tin, Luồng trả lời, Nhắc hẹn, Bình chọn & Nhóm
- **Thao tác trên tin nhắn:** Di chuột vào tin nhắn để *Trả lời trích dẫn*, *Trả lời dạng chuỗi (Thread)*, *Dịch tin nhắn*, *Chuyển tiếp*, *Thả cảm xúc* hoặc bấm chọn nhiều tin nhắn.
- **Gửi ảnh/tệp nhanh:** Nhấn nút ghim kẹp giấy ở khung soạn thảo hoặc nhấn `Ctrl + V` để dán thẳng ảnh chụp màn hình / tệp từ Clipboard.
- **Tạo Nhắc hẹn (Reminder):** Trong cuộc trò chuyện cá nhân hoặc nhóm, bấm biểu tượng **Lịch đồng hồ** trên thanh công cụ soạn thảo để đặt tiêu đề và thời gian nhắc hẹn.
- **Tạo Bình chọn (Poll):** Trong cuộc trò chuyện **Nhóm**, bấm biểu tượng **Biểu đồ cột** trên thanh soạn thảo để tạo bình chọn nhiều lựa chọn, cho phép chọn một hoặc nhiều phương án.
- **Thêm thành viên vào nhóm:** Mở bảng thông tin nhóm (bên phải khung chat) và bấm nút **Thêm** cạnh tiêu đề danh sách thành viên.

### 5. Cài đặt Thông báo nổi 4 góc & Quyền riêng tư
Vào **Cài đặt → Thông báo & Âm thanh**:
- Bật thông báo nổi, chọn **vị trí hiển thị** ở 1 trong 4 góc màn hình (*Trên-trái*, *Trên-phải*, *Dưới-trái*, *Dưới-phải*) và dùng nút **Thử thông báo** để xem trước.
- Khi có thông báo nổi xuất hiện, **bấm trực tiếp vào khung thông báo** để tự động đưa Teza lên trước và mở đúng cuộc hội thoại đó.
- Tùy chỉnh thông báo riêng cho tin nhắn 1-1, tin nhắn nhóm, chế độ *chỉ báo khi được nhắc tên*, chế độ *ẩn nội dung tin nhắn trên thông báo*, âm lượng và kiểu đếm badge chưa đọc.

### 6. Đổi ngôn ngữ giao diện & Dịch tự động
1. Vào **Cài đặt → Giao diện & Chat → Ngôn ngữ giao diện** để chọn **Tiếng Việt**, **English** hoặc **한국어**. Ngôn ngữ đích của tính năng dịch hội thoại sẽ tự động đồng bộ theo ngôn ngữ giao diện bạn chọn.
2. Vào **Cài đặt → Dịch thuật → Cấu hình API** để chọn nhà cung cấp (**DeepL**, **Gemini**, hoặc **Azure Translator**), nhập API key và ngôn ngữ mặc định.
3. Trong khung chat, bạn có thể dịch từng tin nhắn bằng nút biểu tượng ngôn ngữ hoặc bật dịch tự động toàn bộ hội thoại trên thanh tiêu đề phòng chat.

### 7. Tích hợp & mở Web App nhúng
1. Vào **Cài đặt → Web App**, bật tùy chọn hiển thị **Web App** trên thanh bên.
2. Nhập tên hiển thị (ví dụ: *Notion*, *Jira*, *Google Drive*), địa chỉ `https://...` và chọn chế độ:
   - **Mở nhúng trong Teza:** Hiển thị trang web trực tiếp bên trong khung chính của ứng dụng.
   - **Mở bằng trình duyệt ngoài:** Mở bằng trình duyệt mặc định của Windows.
3. Sau khi bật, bấm vào nút **Web App** trên thanh thư mục bên trái để mở bảng quản lý và chuyển đổi nhanh giữa các trang web đã lưu.

### 8. Khóa mã PIN, System Tray & Mạng Relay
- **Khóa mã PIN & Chạy nền:** Vào **Cài đặt → Hệ thống & Phím tắt**:
  - Đặt mã PIN (tối thiểu 4 ký tự) và chọn thời gian tự động khóa khi không thao tác; dùng `Ctrl + L` để khóa màn hình chat ngay lập tức khi rời máy.
  - Bật **Thu nhỏ xuống khay hệ thống khi bấm đóng** để ứng dụng tiếp tục chạy nền nhận tin nhắn trong System Tray.
- **Mạng Relay (Proxy nội bộ):** Vào **Cài đặt → Mạng & Relay**, chọn giao thức (`SOCKS5` hoặc `HTTP CONNECT`), nhập địa chỉ máy chủ/cổng/xác thực (nếu có) và bật định tuyến tải ảnh/avatar/nhãn dán qua Relay nếu cần.

### 9. Bảng phím tắt nhanh

| Phím tắt | Chức năng |
| :--- | :--- |
| `Ctrl + K` | Tìm kiếm nhanh hội thoại và nội dung tin nhắn toàn văn (SQLite FTS5) |
| `Ctrl + F` | Tìm kiếm tin nhắn trong cuộc trò chuyện đang mở |
| `Ctrl + 1` – `Ctrl + 9` | Chuyển nhanh giữa các thư mục trên thanh bên trái |
| `Alt + ↑` / `Alt + ↓` | Di chuyển nhanh lên/xuống giữa các cuộc hội thoại |
| `Ctrl + L` | Khóa ứng dụng ngay bằng mã PIN (khi đã bật khóa PIN) |
| `Esc` | Đóng tuần tự: menu đang mở → tìm kiếm trong chat → bảng luồng trả lời → trích dẫn / chế độ chọn nhiều tin nhắn |
| `Enter` | Gửi tin nhắn *(hoặc xuống dòng nếu tắt chế độ "Nhấn Enter để gửi" trong Cài đặt)* |
| `Shift + Enter` | Xuống dòng khi soạn tin *(dùng `Ctrl + Enter` để gửi khi tắt "Nhấn Enter để gửi")* |
| `Ctrl + V` | Dán trực tiếp ảnh chụp màn hình hoặc tệp từ Clipboard vào khung soạn thảo |
| `@` | Mở danh sách gợi ý nhắc tên thành viên trong nhóm |

---

## Dành cho nhà phát triển

### Yêu cầu môi trường build
- **Node.js** 22+ và **pnpm** 10+
- **Rust** toolchain (`stable`, target Windows MSVC)
- Microsoft Edge WebView2 (có sẵn trên Windows 10/11)

### Cấu trúc Monorepo
- `crates/zalo-core`: Lõi giao thức mạng, mã hóa (`ParamCipher v2`, `signkey`, AES-CBC/GCM), DPAPI, phiên đăng nhập, dịch thuật, proxy client và lưu trữ SQLite + FTS5.
- `crates/litechat-relay`: Binary máy chủ relay trung gian hỗ trợ SOCKS5 và HTTP CONNECT.
- `apps/desktop`: Ứng dụng desktop Tauri v2 (backend Rust tại `apps/desktop/src-tauri` + frontend React/TypeScript/Tailwind tại `apps/desktop/src`).

### Lệnh phát triển & kiểm thử

```powershell
# Cài đặt dependencies frontend
cd apps/desktop
pnpm install

# Chạy chế độ phát triển (dev server / Tauri desktop)
pnpm dev
pnpm tauri dev

# Kiểm thử & build frontend
pnpm test
pnpm build

# Kiểm thử toàn bộ workspace Rust
cd ../..
cargo test --workspace

# Đóng gói bộ cài Windows (.exe NSIS / .msi)
cd apps/desktop
pnpm tauri build
```

---

## Tuyên bố độc lập & Giấy phép

- **Teza** là phần mềm mã nguồn mở độc lập, không liên kết, tài trợ hay đại diện cho bất kỳ nền tảng bên thứ ba nào. Xem chi tiết tại [Pháp lý & Bảo mật](docs/LEGAL_AND_SECURITY.md).
- Phân phối theo giấy phép kép **`MIT OR Apache-2.0`**.
