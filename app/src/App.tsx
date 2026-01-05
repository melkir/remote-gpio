import { useLongPress } from '@uidotdev/usehooks';
import {
  ChevronDown,
  ChevronUp,
  Circle,
  CircleDot,
  Pause,
} from 'lucide-preact';
import { useState } from 'preact/hooks';
import useWebSocket, { ReadyState } from 'react-use-websocket';
import { useHaptic } from 'use-haptic';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

export function App() {
  const [activeLed, setActiveLed] = useState<string | null>(null);
  const { triggerHaptic: shortHaptic } = useHaptic(100);
  const { triggerHaptic: longHaptic } = useHaptic(200);
  const attrs = useLongPress(
    () => {
      send({ command: 'select', led: 'ALL' });
    },
    {
      threshold: 500,
      onStart: () => shortHaptic(),
      onFinish: () => longHaptic(),
    },
  );
  const { sendJsonMessage: send, readyState } = useWebSocket(
    // We cannot use location.host because of Cloudflare Access redirect
    `wss://${location.host}/ws`,
    {
      shouldReconnect: () => true,
      reconnectAttempts: 10,
      //attemptNumber will be 0 the first time it attempts to reconnect, so this equation results in a reconnect pattern of 1 second, 2 seconds, 4 seconds, 8 seconds, and then caps at 10 seconds until the maximum number of attempts is reached
      reconnectInterval: (attemptNumber) =>
        Math.min(2 ** attemptNumber * 1000, 10000),
      queryParams: { name: 'react-app' },
      heartbeat: true,
      onMessage: (event) => {
        if (!event.data || event.data === 'pong') {
          return;
        }
        setActiveLed(event.data);
      },
    },
  );

  // Map readyState to color and label
  const status = {
    [ReadyState.CONNECTING]: 'bg-loading',
    [ReadyState.OPEN]: 'bg-green-900',
    [ReadyState.CLOSING]: 'bg-amber-400',
    [ReadyState.CLOSED]: 'bg-red-900',
    [ReadyState.UNINSTANTIATED]: 'bg-accent',
  }[readyState];

  return (
    <div className="flex min-h-screen flex-col items-center justify-evenly gap-4 pt-4">
      {/* WebSocket Status LED */}
      <div
        className={cn(
          'absolute top-0 h-4 w-72 rounded-b-full bg-accent',
          status,
        )}
      />

      {/* Up, Stop, Down */}
      {[
        {
          icon: <ChevronUp className="size-8 bg-gre" />,
          command: 'up',
          className: 'size-24',
        },
        {
          icon: <Pause className="size-10" />,
          command: 'stop',
          className: 'size-28',
        },
        {
          icon: <ChevronDown className="size-8" />,
          command: 'down',
          className: 'size-24',
        },
      ].map(({ icon, command, className }) => (
        <Button
          {...attrs}
          key={command}
          variant="outline"
          className={cn(className, 'rounded-full active:scale-95')}
          onClick={() => send({ command })}
        >
          {icon}
        </Button>
      ))}

      {/* LED Row */}
      <div className="flex flex-row items-center justify-center gap-12">
        {['L1', 'L2', 'L3', 'L4'].map((led) => (
          <Button
            {...attrs}
            key={led}
            variant="ghost"
            className="size-12 rounded-full active:scale-95"
            onClick={() => send({ command: 'select', led })}
          >
            <Circle
              fill={
                activeLed === 'ALL' || activeLed === led
                  ? 'currentColor'
                  : undefined
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
          onClick={() => send({ command: 'select' })}
          variant="outline"
          className="size-24 rounded-full active:scale-95"
        >
          <CircleDot className="size-8" />
        </Button>
      </div>
    </div>
  );
}
