import { signal } from '@preact/signals';
import { useEffect, useRef } from 'preact/hooks';

export enum ReadyState {
  CONNECTING = 0,
  OPEN = 1,
  CLOSING = 2,
  CLOSED = 3,
}

type Options = {
  queryParams?: Record<string, string>;
  reconnectAttempts?: number;
  reconnectInterval?: number | ((attempt: number) => number);
  onMessage?: (data: string) => void;
  onError?: (event: Event) => void;
};

class WebSocketClient {
  readonly readyState = signal(ReadyState.CLOSED);

  private ws: WebSocket | null = null;
  private socketId: symbol | null = null;
  private reconnectCount = 0;
  private reconnectTimeout?: ReturnType<typeof setTimeout>;
  private shouldReconnect = true;

  constructor(
    private readonly url: string,
    private options: Options = {},
  ) {
    this.connect();
  }

  private getReconnectDelay(attempt: number) {
    const { reconnectInterval = 5000 } = this.options;
    return typeof reconnectInterval === 'function' ? reconnectInterval(attempt) : reconnectInterval;
  }

  private connect() {
    this.ws?.close();

    const ws = new WebSocket(this.url);
    const id = Symbol('ws');

    this.socketId = id;
    this.ws = ws;
    this.readyState.value = ReadyState.CONNECTING;

    ws.onopen = () => {
      if (this.socketId !== id) return;
      this.readyState.value = ReadyState.OPEN;
      this.reconnectCount = 0;
    };

    ws.onmessage = (event) => {
      if (this.socketId !== id) return;
      this.options.onMessage?.(event.data);
    };

    ws.onerror = (event) => {
      if (this.socketId !== id) return;
      this.options.onError?.(event);
    };

    ws.onclose = () => {
      if (this.socketId !== id) return;
      this.readyState.value = ReadyState.CLOSED;
      this.ws = null;

      if (!this.shouldReconnect) return;

      const maxAttempts = this.options.reconnectAttempts ?? 10;
      if (this.reconnectCount >= maxAttempts) return;

      const delay = this.getReconnectDelay(this.reconnectCount++);
      this.reconnectTimeout = setTimeout(() => {
        if (this.shouldReconnect) this.connect();
      }, delay);
    };
  }

  disconnect() {
    this.shouldReconnect = false;
    clearTimeout(this.reconnectTimeout);
    const ws = this.ws;
    this.ws = null;
    this.socketId = null;
    ws?.close();
  }

  reconnect() {
    this.disconnect();
    this.shouldReconnect = true;
    this.reconnectCount = 0;
    this.connect();
  }

  sendJsonMessage(data: unknown) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(data));
    }
  }
}

export function useWebSocket(url: string, options: Options = {}) {
  const clientRef = useRef<WebSocketClient>();

  if (!clientRef.current) {
    const fullUrl = options.queryParams
      ? `${url}?${new URLSearchParams(options.queryParams)}`
      : url;

    clientRef.current = new WebSocketClient(fullUrl, options);
  }

  useEffect(() => {
    return () => clientRef.current?.disconnect();
  }, []);

  const client = clientRef.current;

  return {
    sendJsonMessage: (data: unknown) => client.sendJsonMessage(data),
    readyState: client.readyState,
    reconnect: () => client.reconnect(),
    disconnect: () => client.disconnect(),
  };
}
