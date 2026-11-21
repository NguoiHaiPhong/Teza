# Teza — Kiến trúc Hệ thống

## 1. Mục tiêu sản phẩm
**Teza** là ứng dụng nhắn tin desktop gọn nhẹ cho Windows, tối ưu hiệu năng soạn thảo/đồng bộ và khả năng tùy biến không gian làm việc (bố cục 3 cột, thư mục phân loại, `Ctrl+K` tìm kiếm toàn văn không dấu, Media Lightbox, thông báo nổi 4 góc, dịch tự động, nhắc hẹn, bình chọn và Web App nhúng), giao tiếp trực tiếp thông qua lõi Rust độc lập.

- **Stack UI:** Tauri v2 + React 19 + TypeScript + Tailwind CSS (WebView2 trên Windows 10/11).
- **Lõi giao thức:** Rust crate `zalo-core` (`MIT OR Apache-2.0`), tách biệt hoàn toàn khỏi giao diện.
- **Relay Proxy:** Rust crate `litechat-relay` hỗ trợ SOCKS5 và HTTP CONNECT cho môi trường mạng doanh nghiệp.
- **Lưu trữ cục bộ (Windows):** Mặc định `%LOCALAPPDATA%\Teza\` (hỗ trợ `--data-dir` và `--profile <name>`, tự động di trú dữ liệu từ `%LOCALAPPDATA%\litechat\` nếu tồn tại), tuyệt đối **không** lưu dữ liệu người dùng vào thư mục cài đặt (`Program Files`).

## 2. Cấu trúc Monorepo

```text
litechat/
├── Cargo.toml                  # Rust workspace root (resolver = "2")
├── crates/
│   ├── litechat-relay/         # Máy chủ Relay Proxy (SOCKS5 / HTTP CONNECT)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── zalo-core/              # Tầng giao thức, crypto, SQLite, worker bất đồng bộ
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── endpoints.rs    # Danh sách host hợp lệ
│           ├── crypto.rs       # AES-CBC/GCM, MD5 signkey
│           ├── param_cipher.rs # ParamCipher v2 (dùng CSPRNG getrandom)
│           ├── data_dir.rs     # Phân giải %LOCALAPPDATA%\Teza\ & ghi atomic
│           ├── dpapi.rs        # Mã hóa khóa phiên trên Windows bằng DPAPI
│           ├── relay.rs        # Cấu hình & kết nối Relay Proxy
│           ├── translate.rs    # Cấu hình DeepL / Gemini / Azure Translator (niêm phong DPAPI)
│           ├── session.rs      # Quản lý phiên, cookie jar, phân biệt lỗi mạng vs 600/-102
│           ├── qr.rs           # Đăng nhập QR
│           ├── login.rs        # Khởi tạo phiên & getServerInfo/getLoginInfo
│           ├── socket.rs       # WebSocket (AES-GCM/deflate)
│           ├── store.rs        # SQLite + FTS5 tìm kiếm không dấu
│           └── chat/           # Các module xử lý hội thoại, tin nhắn & parser
├── apps/
│   └── desktop/                # Tauri v2 Desktop App (productName: Teza)
│       ├── package.json
│       ├── src/                # React + TypeScript + Tailwind UI + i18n (vi/en/ko)
│       └── src-tauri/          # Lớp cầu nối Tauri Commands, Event Stream & Child WebViews
└── docs/
    ├── ARCHITECTURE.md
    └── LEGAL_AND_SECURITY.md
```

## 3. Ranh giới IPC (`CoreBridge`)

Mọi giao tiếp giữa Frontend (WebView2) và Backend (`zalo-core`) đi qua **Tauri Commands + Typed Event Stream**, **không** mở cổng HTTP REST với `Access-Control-Allow-Origin: *`:

### Commands (UI -> Core)
- `auth_check_session()` -> `SessionState` (`LoggedOut | NeedsQr | Authenticated { uin, name, avatar }`)
- `auth_start_qr()` -> trả về `qr_png_base64` / SVG + bắt đầu poll nền
- `auth_logout()` -> xóa phiên xác thực (giữ hoặc xóa DB theo tùy chọn)
- `threads_list(folder_filter)` -> danh sách hội thoại + unread count
- `messages_load(thread_id, before_ts, limit)` -> tin nhắn phân trang từ SQLite / server
- `message_send_text(thread_id, text, quote_id?)`
- `message_send_attachment(thread_id, file_path)`
- `message_react(thread_id, msg_id, reaction)`
- `message_recall(thread_id, msg_id)`
- `search_global(query)` -> kết quả hội thoại + tin nhắn (SQLite FTS5)

### Events (Core -> UI)
- `core://auth-state` (`QrScanned`, `Authenticated`, `SessionExpired`, `NetworkError`)
- `core://message-new` (`MessageDto`)
- `core://message-updated` (`msg_id`, reactions / recalled)
- `core://thread-updated` (`ThreadDto`)
- `core://connection-status` (`Connected`, `Reconnecting`, `Offline`)
