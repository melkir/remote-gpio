import { signal } from '@preact/signals';
import { useEffect, useRef } from 'preact/hooks';

export enum ReadyState {
  CONNECTING = 0,
  OPEN = 1,
  CLOSED = 2,
}

const DOWN_AFTER_MS = 30_000;

type Options = {
  onSelection?: (selection: string) => void;
  onError?: (event: Event) => void;
};

export function useSelectionEvents(url: string, options: Options = {}) {
  const readyState = useRef(signal(ReadyState.CLOSED)).current;
  const optsRef = useRef(options);
  optsRef.current = options;

  useEffect(() => {
    const es = new EventSource(url);
    let downTimer: ReturnType<typeof setTimeout> | null = null;
    readyState.value = ReadyState.CONNECTING;

    const clearDown = () => {
      if (downTimer === null) return;
      clearTimeout(downTimer);
      downTimer = null;
    };

    es.onopen = () => {
      clearDown();
      readyState.value = ReadyState.OPEN;
    };

    es.addEventListener('selection', (event) => {
      optsRef.current.onSelection?.(event.data);
    });

    es.onerror = (event) => {
      if (es.readyState === EventSource.CLOSED) {
        clearDown();
        readyState.value = ReadyState.CLOSED;
      } else {
        readyState.value = ReadyState.CONNECTING;
        if (downTimer === null) {
          downTimer = setTimeout(() => {
            downTimer = null;
            readyState.value = ReadyState.CLOSED;
          }, DOWN_AFTER_MS);
        }
      }
      optsRef.current.onError?.(event);
    };

    return () => {
      clearDown();
      es.close();
      readyState.value = ReadyState.CLOSED;
    };
  }, [url]);

  return { readyState };
}
