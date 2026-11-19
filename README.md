# litechat

Client nhắn tin desktop gọn nhẹ lấy cảm hứng từ thiết kế tối giản của Telegram, chạy trên lõi Rust (`zalo-core`) + giao diện Tauri v2 (React/TypeScript/Tailwind).

## Kiến trúc
- **`crates/zalo-core`**: Lõi giao thức mạng, mã hóa (`ParamCipher v2`, `signkey`), quản lý phiên (DPAPI trên Windows), lưu trữ SQLite + FTS5.
- **`apps/desktop`**: Giao diện Tauri v2 phong cách Telegram (3 cột, Chat Folders, Media Lightbox, tìm kiếm `Ctrl+K`, System Tray).
- **Tài liệu:** [Kiến trúc hệ thống](docs/ARCHITECTURE.md) · [Pháp lý & Bảo mật](docs/LEGAL_AND_SECURITY.md)

## Giấy phép
`MIT OR Apache-2.0`
