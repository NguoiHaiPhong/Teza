# Teza Google Drive Zero-Secret OAuth Token Broker

Cloudflare Worker (~130 dòng, chạy miễn phí trên Cloudflare Workers Free Tier — 100.000 request/ngày) giúp bản phát hành Teza Desktop hỗ trợ **Đăng nhập Google 1 chạm** mà **không phải nhúng `GOOGLE_CLIENT_SECRET` vào file `.exe`**.

## Cơ chế hoạt động
1. Teza mở thẳng trang đăng nhập Google chính chủ (`accounts.google.com`) với PKCE `S256` + `state` + loopback `http://127.0.0.1:<port>/callback`.
2. Sau khi Google trả `code` về Teza, Teza gửi `{ client_id, code, code_verifier, redirect_uri }` lên `POST /token` của Worker này.
3. Worker kiểm tra chặt `client_id`, chuẩn PKCE `code_verifier`, và `redirect_uri` phải là loopback `127.0.0.1:<port>/callback`, rồi ghép thêm `GOOGLE_CLIENT_SECRET` ở phía server và chuyển tiếp tới `https://oauth2.googleapis.com/token`.
4. Worker **không lưu trữ**, **không ghi log token**, và **không bao giờ chạm vào file Google Drive hay khóa mã hóa E2EE** của người dùng.

## Triển khai trong 2 phút
```powershell
cd infra/gdrive-oauth-broker
npx wrangler login
npx wrangler secret put GOOGLE_CLIENT_ID
npx wrangler secret put GOOGLE_CLIENT_SECRET
npx wrangler deploy
```

Sau khi deploy xong (ví dụ nhận được `https://teza-gdrive-oauth-broker.<subdomain>.workers.dev`), kiểm tra sức khỏe:
```powershell
curl https://teza-gdrive-oauth-broker.<subdomain>.workers.dev/health
```
Kết quả `{"ok":true,"configured":true}` là sẵn sàng. Tiếp theo thêm 2 GitHub Actions Secrets vào repo:
- `TEZA_GDRIVE_CLIENT_ID` = Desktop OAuth Client ID của bạn
- `TEZA_GDRIVE_TOKEN_BROKER_URL` = URL Worker vừa deploy (để trống `TEZA_GDRIVE_CLIENT_SECRET`)
