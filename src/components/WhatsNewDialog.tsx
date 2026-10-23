import { ArrowUpRight, Sparkles } from "lucide-react";
import { Button, Dialog } from "@/components/ui";
import type { Route } from "@/app/routes";
import type { WhatsNewEntry } from "@/lib/whatsNew";

/** What changed in WordFlow, shown once after an update and from Settings. */
export function WhatsNewDialog({
  entries,
  onClose,
  onNavigate,
}: {
  entries: WhatsNewEntry[] | null;
  onClose: () => void;
  onNavigate?: (route: Route) => void;
}) {
  return (
    <Dialog
      open={Boolean(entries?.length)}
      onClose={onClose}
      width={480}
      title="What's new in WordFlow"
      footer={
        <Button variant="primary" onClick={onClose}>
          Got it
        </Button>
      }
    >
      <ul className="flex flex-col gap-4">
        {entries?.map((entry) => (
          <li key={entry.id} className="flex gap-3">
            <Sparkles size={16} className="mt-0.5 shrink-0 text-accent-ink" />
            <div className="min-w-0 flex-1">
              <div className="type-label">{entry.title}</div>
              <p className="mt-0.5 type-small text-ink-secondary">{entry.description}</p>
              {entry.route && onNavigate && (
                <button
                  type="button"
                  onClick={() => {
                    onNavigate(entry.route!);
                    onClose();
                  }}
                  className="mt-1 flex items-center gap-1 type-caption font-medium text-ink-secondary transition-colors hover:text-ink"
                >
                  Take a look <ArrowUpRight size={12} />
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </Dialog>
  );
}
