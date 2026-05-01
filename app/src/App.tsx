import { Button } from '@/components/ui/button';
import { ReadyState, useSelectionEvents } from '@/hooks/use-selection-events';
import { cn } from '@/lib/utils';
import { useLongPress } from '@uidotdev/usehooks';
import { ChevronDown, ChevronUp, Circle, CircleDot, Pause } from 'lucide-preact';
import { useCallback, useEffect, useState } from 'preact/hooks';
import { useHaptic } from 'use-haptic';

export function App() {
  const [activeLed, setActiveLed] = useState<string | null>(null);
  const { triggerHaptic: shortHaptic } = useHaptic(100);
  const { triggerHaptic: longHaptic } = useHaptic(200);
  const send = useCallback(async (payload: { command: string; channel?: string }) => {
    const response = await fetch('/command', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(payload),
    });

    if (!response.ok) {
      console.warn('[Command] Request failed:', await response.text());
    }
  }, []);
  const { readyState } = useSelectionEvents('/events', {
    onSelection: setActiveLed,
  });

  useEffect(() => {
    const controller = new AbortController();

    async function syncSelection() {
      try {
        const response = await fetch('/channel', {
          cache: 'no-store',
          signal: controller.signal,
        });

        if (response.ok) {
          const channel = (await response.text()).trim();
          if (channel) {
            setActiveLed(channel);
          }
        }
      } catch (error) {
        if (!controller.signal.aborted) {
          console.warn('[Selection] Snapshot request failed:', error);
        }
      }
    }

    syncSelection();

    return () => controller.abort();
  }, []);

  const attrs = useLongPress(
    () => {
      send({ command: 'select', channel: 'ALL' });
    },
    {
      threshold: 500,
      onStart: () => shortHaptic(),
      onFinish: () => longHaptic(),
    },
  );

  const status = {
    [ReadyState.CONNECTING]: 'bg-loading',
    [ReadyState.OPEN]: 'bg-green-900',
    [ReadyState.CLOSED]: 'bg-red-900',
  }[readyState.value];

  return (
    <div className="flex min-h-screen flex-col items-center justify-evenly gap-4 pt-4">
      {/* Connection Status LED */}
      <div className={cn('absolute top-0 h-4 w-72 rounded-b-full bg-accent', status)} />

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

      {/* LED Row */}
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
              fill={activeLed === 'ALL' || activeLed === channel ? 'currentColor' : undefined}
              className="size-6"
            />
          </Button>
        ))}
      </div>

      {/* Center Select Button */}
      <div className="flex flex-row items-center justify-center">
        <Button
          {...attrs}
          onClick={() => send({ command: 'select' })}
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
