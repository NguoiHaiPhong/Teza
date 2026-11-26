# Hướng dẫn bật Đăng nhập Google Drive 1 chạm cho Teza

Người dùng phổ thông **không cần** tự tạo Google OAuth Client ID. Bạn (nhà phát triển) chỉ cần đăng ký **1 OAuth Desktop Client duy nhất** cho Teza một lần, sau đó gắn vào bản build phát hành (CI/CD). Mọi người dùng chỉ cần bấm **“Đăng nhập bằng Google”** 1 lần là sử dụng được ngay Google Drive E2EE.

---

## Bước 1: Tạo Google OAuth Desktop Client (khoảng 3 phút)

1. Mở [Google Cloud Console](https://console.cloud.google.com/) → tạo Project mới (ví dụ: `Teza Desktop`).
2. Vào **APIs & Services → Library** → tìm **Google Drive API** → bấm **Enable**.
3. Vào **Google Auth Platform (OAuth consent screen)**:
   - Chọn **User Type: External**.
   - Điền **App name** (`Teza`), **User support email**, **Developer contact email**.
   - Vào **Data Access / Scopes** → thêm duy nhất scope:
     - `https://www.googleapis.com/auth/drive.file`
     *(Đây là **non-sensitive scope** — Teza chỉ thấy thư mục `Teza Vault/` do chính Teza tạo ra và **không phải trả phí kiểm toán CASA**.)*
4. Vào **Clients (Credentials) → Create OAuth client ID**:
   - **Application type**: chọn **Desktop app** (bắt buộc để Google cho phép loopback `http://127.0.0.1:<port>/callback`).
   - Lưu lại **Client ID** (`...apps.googleusercontent.com`) và **Client Secret** (`GOCSPX-...`).

---

## Bước 2: Chọn 1 trong 2 cách gắn vào bản phát hành Teza

### Cách 1 (Khuyến nghị — Bảo mật tối đa, không lộ `Client Secret` trong file `.exe`)
Triển khai Worker trung gian đổi token miễn phí tại [`infra/gdrive-oauth-broker`](../infra/gdrive-oauth-broker/README.md) (Cloudflare Workers Free Tier):
1. Chạy `npx wrangler secret put GOOGLE_CLIENT_ID`, `npx wrangler secret put GOOGLE_CLIENT_SECRET`, và `npx wrangler deploy` trong `infra/gdrive-oauth-broker/`.
2. Vào **GitHub Repo → Settings → Secrets and variables → Actions** và thêm 2 Secret:
   - `TEZA_GDRIVE_CLIENT_ID` = Client ID từ Bước 1
   - `TEZA_GDRIVE_TOKEN_BROKER_URL` = URL Worker HTTPS (ví dụ: `https://teza-gdrive-oauth-broker.xxx.workers.dev`)
   - *(Không cần đặt `TEZA_GDRIVE_CLIENT_SECRET` — file `.exe` hoàn toàn không chứa secret!)*

### Cách 2 (Nhanh nhất — Không cần Cloudflare)
Vào **GitHub Repo → Settings → Secrets and variables → Actions** và thêm 2 Secret:
- `TEZA_GDRIVE_CLIENT_ID` = Client ID từ Bước 1
- `TEZA_GDRIVE_CLIENT_SECRET` = Client Secret từ Bước 1

Workflow [`.github/workflows/release.yml`](../.github/workflows/release.yml) đã tự động truyền cả 3 biến môi trường này khi build bản phát hành.

> **Test cục bộ khi dev**: đặt biến môi trường `TEZA_GDRIVE_CLIENT_ID` (và `TEZA_GDRIVE_TOKEN_BROKER_URL` hoặc `TEZA_GDRIVE_CLIENT_SECRET`) trước khi chạy `cargo build` / `pnpm tauri dev`, hoặc mở mục **Cấu hình OAuth nâng cao (Dành cho nhà phát triển)** trong **Cài đặt → Lưu trữ & Media**.

---

## Bước 3: Xuất bản cho mọi tài khoản Gmail (Brand Verification miễn phí)

- Khi ở trạng thái **Testing**, chỉ tối đa 100 email được thêm thủ công trong mục *Test users* mới đăng nhập được, và refresh token hết hạn sau **7 ngày**.
- Để **mọi tài khoản Gmail** đều đăng nhập 1 chạm, refresh token sống lâu dài và không hiện cảnh báo *"Unverified app"*:
  1. Chuẩn bị 1 trang web giới thiệu + Privacy Policy (có thể dùng ngay GitHub Pages của repo).
  2. Trong Google Auth Platform → **Audience** → bấm **Publish App** (Production).
  3. Gửi **Brand Verification** miễn phí cho scope non-sensitive `drive.file` (thường được Google duyệt trong vài ngày, không mất phí kiểm toán bảo mật).

---

## Ghi chú về mạng LAN (Relay)
- Khi bật **Relay** trong **Cài đặt → Mạng & Relay**, toàn bộ luồng Google Drive (kể cả OAuth & Token Broker) tự động tạm khóa và không đi qua `litechat-relay`.
