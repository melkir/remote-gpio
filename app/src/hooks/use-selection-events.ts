import { signal } from '@preact/signals';
import { useEffect, useRef } from 'preact/hooks';

export enum ReadyState {
  CONNECTING = 0,
  OPEN = 1,
  CLOSED = 2,
}

type Options = {
  onSelection?: (selection: string) => void;
  onError?: (event: Event) => void;
};

class SelectionEventClient {
  readonly readyState = signal(ReadyState.CLOSED);

  private eventSource: EventSource | null = null;

  constructor(
    private readonly url: string,
    private readonly options: Options = {},
  ) {
    this.connect();
  }

  private connect() {
    const events = new EventSource(this.url);

    this.eventSource = events;
    this.readyState.value = ReadyState.CONNECTING;

    events.onopen = () => {
      this.readyState.value = ReadyState.OPEN;
    };

    events.addEventListener('selection', (event) => {
      this.options.onSelection?.(event.data);
    });

    events.onerror = (event) => {
      this.readyState.value =
        events.readyState === EventSource.CLOSED ? ReadyState.CLOSED : ReadyState.CONNECTING;
      this.options.onError?.(event);
    };
  }

  disconnect() {
    this.eventSource?.close();
    this.eventSource = null;
    this.readyState.value = ReadyState.CLOSED;
  }
}

export function useSelectionEvents(url: string, options: Options = {}) {
  const clientRef = useRef<SelectionEventClient>();

  if (!clientRef.current) {
    clientRef.current = new SelectionEventClient(url, options);
  }

  useEffect(() => {
    return () => clientRef.current?.disconnect();
  }, []);

  return {
    readyState: clientRef.current.readyState,
    disconnect: () => clientRef.current?.disconnect(),
  };
}
