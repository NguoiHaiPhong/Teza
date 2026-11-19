import { QrCode, RefreshCw } from "lucide-react";

interface Props {
  qrSvg: string;
  statusText: string;
  errorText: string | null;
  onRefresh: () => void;
  onEnterDemo: () => void;
}

export function QrLoginModal({
  qrSvg,
  statusText,
  errorText,
  onRefresh,
  onEnterDemo,
}: Props) {
  return (
    <div className="flex h-full w-full items-center justify-center p-6" style={{ background: "var(--bg-stage)" }}>
      <div
        className="w-full max-w-md rounded-2xl p-8 text-center shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
      >
        <div
          className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full"
          style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
        >
          <QrCode className="h-6 w-6" />
        </div>
        <h1 className="text-xl font-semibold">Đăng nhập litechat</h1>
        <p className="mt-1 text-sm" style={{ color: "var(--text-secondary)" }}>
          Mở ứng dụng trên điện thoại và quét mã QR bên dưới
        </p>

        <div className="mx-auto my-6 flex h-56 w-56 items-center justify-center rounded-xl bg-white p-3">
          {qrSvg ? (
            <div className="h-full w-full [&>svg]:h-full [&>svg]:w-full" dangerouslySetInnerHTML={{ __html: qrSvg }} />
          ) : (
            <div className="text-xs text-slate-500">Đang tải mã QR từ máy chủ...</div>
          )}
        </div>

        <p className="text-sm font-medium" style={{ color: "var(--accent)" }}>
          {statusText}
        </p>
        {errorText && (
          <p
            className="mt-2 rounded-lg px-3 py-2 text-xs"
            style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
          >
            {errorText}
          </p>
        )}

        <div className="mt-6 flex items-center justify-center gap-3">
          <button
            onClick={onRefresh}
            className="tf-btn inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-semibold"
          >
            <RefreshCw className="h-4 w-4" />
            Tạo lại QR
          </button>
          <button
            onClick={onEnterDemo}
            className="rounded-lg px-4 py-2 text-sm font-medium transition hover:opacity-80"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            Xem trước giao diện
          </button>
        </div>
      </div>
    </div>
  );
}
