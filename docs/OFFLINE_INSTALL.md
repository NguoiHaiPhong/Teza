# Hướng dẫn cài đặt Teza trên máy Windows không có Internet (Offline)

## Vì sao máy không có Internet thường báo lỗi thiếu thư viện?

- Bộ cài mặc định của **Teza** (`Teza_<phiên_bản>_x64-setup.exe` và `.msi`) sử dụng **Microsoft Edge WebView2 Runtime** để hiển thị giao diện. Nếu máy chưa có sẵn WebView2, bộ cài sẽ gọi **Bootstrapper** của Microsoft để tự tải từ Internet — trên máy offline bước này sẽ bị treo hoặc thất bại.
- Trên một số máy **Windows 10 đời cũ, Windows LTSC hoặc Windows Server** chưa cập nhật đầy đủ gói hệ thống, ứng dụng cũng có thể báo thiếu **Microsoft Visual C++ 2015–2022 Redistributable (x64)** / Universal CRT (`VCRUNTIME140.dll`, `VCRUNTIME140_1.dll`, `MSVCP140.dll`, `api-ms-win-crt-runtime-l1-1-0.dll`).

> Lưu ý: **SQLite** và **TLS/Crypto** đã được đóng gói tĩnh bên trong tệp thực thi của Teza nên không cần cài thêm bên ngoài.

---

## 1. Danh sách tệp cần tải sẵn trên máy có Internet

Hãy tải đầy đủ **3 tệp** sau về USB hoặc thư mục mạng nội bộ (LAN):

| Thành phần | Tệp cài đặt | Dung lượng | Link tải trực tiếp (chính thức từ Microsoft / GitHub) | Ghi chú |
| :--- | :--- | :---: | :--- | :--- |
| **1. Microsoft Edge WebView2 Runtime** *(Evergreen Standalone — Offline x64)* | `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` | ~280 MB | **[Tải trực tiếp (Microsoft)](https://go.microsoft.com/fwlink/?linkid=2124701)** | Bắt buộc dùng bản **Evergreen Standalone Installer (x64)**. **Không** tải *Evergreen Bootstrapper* (~1.7 MB) vì bản đó cần Internet khi cài. Trang chủ tải thủ công: [Microsoft Edge WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) |
| **2. Visual C++ 2015–2022 Redistributable** *(x64)* | `VC_redist.x64.exe` | ~24.4 MB | **[Tải trực tiếp (Microsoft)](https://aka.ms/vs/17/release/vc_redist.x64.exe)** | Cần cho Windows 10 đời cũ, LTSC, Windows Server hoặc máy thiếu DLL `VCRUNTIME140*.dll`. Trang tài liệu: [Microsoft Docs VC++ Redist](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist) |
| **3. Bộ cài Teza** *(x64)* | `Teza_<phiên_bản>_x64-setup.exe` hoặc `.msi` | ~8–15 MB | **[Trang Releases mới nhất](https://github.com/NguoiHaiPhong/Teza/releases/latest)** | Khuyến nghị dùng bản `*-setup.exe` cho cài đặt cá nhân hoặc `.msi` cho triển khai hàng loạt |

---

## 2. Kiểm tra nhanh máy đã có WebView2 Runtime hay chưa (tùy chọn)

Trên máy không có Internet, mở **PowerShell** và chạy lệnh dưới đây để kiểm tra Registry theo đúng mã định danh chính thức `{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`:

```powershell
@(
  "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
  "HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
) | ForEach-Object {
  if (Test-Path $_) {
    $ver = (Get-ItemProperty $_ -ErrorAction SilentlyContinue).pv
    if ($ver) { Write-Output "Đã cài WebView2: $ver ($_)" }
  }
}
```

- Nếu lệnh **không in ra dòng nào** (hoặc giá trị `pv` trống), bạn cần cài `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` trước khi cài Teza.

---

## 3. Thứ tự cài đặt trên máy không có Internet

1. **Chép toàn bộ tệp đã tải** (`VC_redist.x64.exe`, `MicrosoftEdgeWebView2RuntimeInstallerX64.exe`, và bộ cài `Teza`) sang máy đích qua USB hoặc mạng LAN.
2. **Cài Visual C++ 2015–2022 Redistributable (x64):**
   - Nhấp đúp `VC_redist.x64.exe` (chạy bằng quyền Administrator nếu được hỏi) → hoàn tất quá trình cài.
   - *(Nếu máy báo đã có bản mới hơn, có thể bỏ qua bước này.)*
3. **Cài Microsoft Edge WebView2 Runtime (Offline):**
   - Chuột phải vào `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` → chọn **Run as administrator** để cài **Per-Machine** cho toàn bộ tài khoản Windows trên máy.
   - Đợi tiến trình cài ngầm hoàn tất (thường mất 15–40 giây; có thể kiểm tra lại bằng lệnh PowerShell ở Mục 2).
4. **Cài Teza:**
   - Chạy `Teza_<phiên_bản>_x64-setup.exe` (hoặc `Teza_<phiên_bản>_x64_en-US.msi`).
5. **Cấu hình Mạng Relay (nếu máy không ra thẳng được Internet):**
   - Mở Teza, ngay tại màn hình đăng nhập QR bấm **Mạng Relay** để trỏ tới máy chủ `litechat-relay` trong mạng nội bộ.
   - Xem hướng dẫn dựng máy chủ Relay trên Windows tại [deploy/relay-windows/README.md](../deploy/relay-windows/README.md).

---

## 4. Cài đặt tự động (Silent Install) cho IT / nhiều máy

Mở **PowerShell (Run as Administrator)** trong thư mục chứa 3 tệp cài đặt và chạy lần lượt:

```powershell
# 1. Cài Visual C++ 2015-2022 Redistributable x64
Start-Process -FilePath ".\VC_redist.x64.exe" -ArgumentList "/install /quiet /norestart" -Wait

# 2. Cài WebView2 Runtime Offline (Per-Machine vì chạy dưới quyền Admin)
Start-Process -FilePath ".\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" -ArgumentList "/silent /install" -Wait

# 3. Cài Teza (chọn 1 trong 2 dòng tương ứng với bộ cài bạn tải)
Start-Process -FilePath ".\Teza_0.5.0_x64-setup.exe" -ArgumentList "/S" -Wait
# Hoặc nếu dùng gói MSI:
# Start-Process -FilePath "msiexec.exe" -ArgumentList "/i Teza_0.5.0_x64_en-US.msi /qn" -Wait
```

---

## 5. Xử lý nhanh các lỗi thường gặp

| Triệu chứng | Nguyên nhân | Cách khắc phục |
| :--- | :--- | :--- |
| Bộ cài Teza bị treo hoặc báo lỗi mạng ở bước tải WebView2 | Máy offline và chưa có sẵn WebView2 Runtime | Hủy bộ cài Teza, cài `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` trước rồi chạy lại bộ cài Teza |
| Đã tải WebView2 nhưng chạy vẫn đòi Internet / lỗi kết nối | Tải nhầm tệp `MicrosoftEdgeWebview2Setup.exe` (~1.7 MB, Evergreen Bootstrapper) | Tải lại đúng bản **Evergreen Standalone Installer (x64)** ~280 MB theo link trực tiếp ở Mục 1 |
| Lỗi `The code execution cannot proceed because VCRUNTIME140.dll / MSVCP140.dll / api-ms-win-crt-runtime-l1-1-0.dll was not found` | Thiếu gói Visual C++ Runtime / Universal CRT trên Windows 10 cũ hoặc LTSC/Server | Cài `VC_redist.x64.exe` rồi khởi động lại máy nếu được yêu cầu |
| Cài xong mở Teza lên bị trắng màn hình hoặc không mở cửa sổ | WebView2 bị hỏng hoặc chỉ cài cho user khác trên cùng máy | Chạy lại `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` bằng **Run as administrator** |
