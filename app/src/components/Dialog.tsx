import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { X } from "lucide-react";

interface ModalProps {
  title: string;
  children: ReactNode;
  /** Footer buttons. A `type="submit"` button submits the form. */
  actions: ReactNode;
  onSubmit(): void;
  onClose(): void;
  className?: string;
}

/** Modal built on <dialog>: focus is trapped, Escape closes, and the page
 *  behind is inert. */
export function Modal({ title, children, actions, onSubmit, onClose, className }: ModalProps) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (d && !d.open) d.showModal();
    // showModal() focuses the first focusable (the close button); prefer the marked field.
    d?.querySelector<HTMLElement>("[data-autofocus]")?.focus();
    return () => d?.close();
  }, []);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onSubmit();
  };
  return (
    <dialog
      ref={ref}
      className={`modal ${className ?? ""}`}
      aria-label={title}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onMouseDown={(e) => {
        // Click on the backdrop (the dialog element itself, outside the form).
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <form onSubmit={submit}>
        <header className="modal-head">
          <h2>{title}</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="modal-body">{children}</div>
        <footer className="modal-foot">{actions}</footer>
      </form>
    </dialog>
  );
}

interface ConfirmProps {
  title: string;
  message: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onConfirm(): Promise<void> | void;
  onClose(): void;
}

export function ConfirmDialog({ title, message, confirmLabel, danger, onConfirm, onClose }: ConfirmProps) {
  const [error, setError] = useState<string | null>(null);
  const confirm = async () => {
    try {
      await onConfirm();
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <Modal
      title={title}
      className="modal-confirm"
      onSubmit={confirm}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className={`btn ${danger ? "btn-danger" : "btn-primary"}`} data-autofocus>
            {confirmLabel}
          </button>
        </>
      }
    >
      <p>{message}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
