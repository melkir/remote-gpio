import { Button } from '@/components/ui/button';
import { ReadyState, useSelectionEvents } from '@/hooks/use-selection-events';
import { handleAccessSessionExpiry, isAccessChallenge, isValidChannel } from '@/lib/access';
import { cn } from '@/lib/utils';
import { useLongPress } from '@uidotdev/usehooks';
import { ChevronDown, ChevronUp, Circle, CircleDot, Pause } from 'lucide-preact';
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';
import { useHaptic } from 'use-haptic';

export function App() {
  const [activeChannel, setActiveChannel] = useState<string | null>(null);
  const { triggerHaptic: shortHaptic } = useHaptic(100);
  const { triggerHaptic: longHaptic } = useHaptic(200);
  const send = useCallback(async (payload: { command: string; channel?: string }) => {
    try {
      const response = await fetch('/command', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });

      if (isAccessChallenge(response)) {
        await handleAccessSessionExpiry();
        return;
      }

      if (!response.ok) {
        console.warn('[Command] Request failed:', await response.text());
      }
    } catch (error) {
      console.warn('[Command] Request failed:', error);
    }
  }, []);
  const { readyState } = useSelectionEvents('/events', {
    onSelection: setActiveChannel,
    onClosed: handleAccessSessionExpiry,
  });

  useEffect(() => {
    const controller = new AbortController();

    async function syncSelection() {
      try {
        const response = await fetch('/channel', {
          cache: 'no-store',
          signal: controller.signal,
        });

        if (isAccessChallenge(response)) {
          await handleAccessSessionExpiry();
          return;
        }

        if (response.ok) {
          const channel = (await response.text()).trim();
          if (isValidChannel(channel)) {
            setActiveChannel(channel);
          } else {
            await handleAccessSessionExpiry();
          }
        }
      } catch (error) {
        if (controller.signal.aborted) return;
        console.warn('[Selection] Snapshot request failed:', error);
      }
    }

    syncSelection();

    return () => controller.abort();
  }, []);

  // `useLongPress` only wires pointer handlers — it never suppresses the click
  // the browser dispatches on release. Without this guard a long press sends
  // `select ALL` and the trailing click immediately cycles the selection off it.
  const longPressFired = useRef(false);
  const attrs = useLongPress(
    () => {
      longPressFired.current = true;
      send({ command: 'select', channel: 'ALL' });
    },
    {
      threshold: 500,
      onStart: () => {
        // Every new press starts clean, so a long press that never produced a
        // click (pointer left the button) cannot swallow the next real one.
        longPressFired.current = false;
        shortHaptic();
      },
      onFinish: () => longHaptic(),
    },
  );

  const cycleSelection = useCallback(() => {
    if (longPressFired.current) {
      longPressFired.current = false;
      return;
    }
    send({ command: 'select' });
  }, [send]);

  const status = {
    [ReadyState.CONNECTING]: { className: 'bg-loading', label: 'Connecting to blinds' },
    [ReadyState.OPEN]: { className: 'bg-green-900', label: 'Connected to blinds' },
    [ReadyState.CLOSED]: { className: 'bg-red-900', label: 'Disconnected from blinds' },
  }[readyState.value];

  return (
    <div className="flex min-h-screen flex-col items-center justify-evenly gap-4 pt-4">
      {/* Connection status indicator */}
      <div
        role="status"
        aria-label={status.label}
        className={cn('absolute top-0 h-4 w-72 rounded-b-full bg-accent', status.className)}
      />

      {/* Up, Stop, Down */}
      {[
        {
          icon: <ChevronUp className="size-8" />,
          command: 'up',
          label: 'Move up',
          className: 'size-24',
        },
        {
          icon: <Pause className="size-10" />,
          command: 'stop',
          label: 'Stop',
          className: 'size-28',
        },
        {
          icon: <ChevronDown className="size-8" />,
          command: 'down',
          label: 'Move down',
          className: 'size-24',
        },
      ].map(({ icon, command, label, className }) => (
        <Button
          key={command}
          variant="outline"
          className={cn(className, 'rounded-full active:scale-95')}
          aria-label={label}
          onClick={() => send({ command })}
        >
          {icon}
        </Button>
      ))}

      {/* Channel selection row */}
      <div className="flex flex-row items-center justify-center gap-12">
        {['L1', 'L2', 'L3', 'L4'].map((channel) => (
          <Button
            key={channel}
            variant="ghost"
            className="size-12 rounded-full active:scale-95"
            aria-label={`Select ${channel}`}
            onClick={() => send({ command: 'select', channel })}
          >
            <Circle
              fill={
                activeChannel === 'ALL' || activeChannel === channel ? 'currentColor' : undefined
              }
              className="size-6"
            />
          </Button>
        ))}
      </div>

      {/* Center Select Button */}
      <div className="flex flex-row items-center justify-center">
        <Button
          {...attrs}
          onClick={cycleSelection}
          variant="outline"
          className="size-24 rounded-full active:scale-95"
          aria-label="Cycle selection (long press to select all)"
        >
          <CircleDot className="size-8" />
        </Button>
      </div>
    </div>
  );
}
