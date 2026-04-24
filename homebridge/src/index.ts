import type {
  AccessoryPlugin,
  API,
  CharacteristicValue,
  Logging,
  Service,
  StaticPlatformPlugin,
} from 'homebridge';

const PLUGIN_NAME = 'homebridge-somfy-remote';
const PLATFORM_NAME = 'SomfyRemote';

type Led = 'L1' | 'L2' | 'L3' | 'L4' | 'ALL';

interface BlindConfig {
  name: string;
  led: Led;
}

interface PlatformConfig {
  name?: string;
  baseUrl?: string;
  blinds?: BlindConfig[];
  requestTimeoutMs?: number;
}

const DEFAULT_BLINDS: BlindConfig[] = [
  { name: 'Blind 1', led: 'L1' },
  { name: 'Blind 2', led: 'L2' },
  { name: 'Blind 3', led: 'L3' },
  { name: 'Blind 4', led: 'L4' },
  { name: 'All Blinds', led: 'ALL' },
];

const VALID_LEDS: ReadonlySet<Led> = new Set(['L1', 'L2', 'L3', 'L4', 'ALL']);

function isBlindConfig(candidate: unknown): candidate is BlindConfig {
  if (typeof candidate !== 'object' || candidate === null) return false;
  const value = candidate as Record<string, unknown>;
  return (
    typeof value.name === 'string' &&
    typeof value.led === 'string' &&
    VALID_LEDS.has(value.led as Led)
  );
}

class SomfyRemotePlatform implements StaticPlatformPlugin {
  private readonly log: Logging;
  private readonly api: API;
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly blinds: BlindConfig[];

  constructor(log: Logging, config: PlatformConfig, api: API) {
    this.log = log;
    this.api = api;
    this.baseUrl = config.baseUrl ?? 'http://localhost:5002';
    this.timeoutMs = config.requestTimeoutMs ?? 5000;

    const configured =
      Array.isArray(config.blinds) && config.blinds.length > 0
        ? config.blinds
        : DEFAULT_BLINDS;

    this.blinds = configured.filter((entry): entry is BlindConfig => {
      if (!isBlindConfig(entry)) {
        this.log.warn(`ignoring invalid blind entry: ${JSON.stringify(entry)}`);
        return false;
      }
      return true;
    });
  }

  accessories(callback: (found: AccessoryPlugin[]) => void): void {
    callback(
      this.blinds.map(
        (blind) => new SomfyBlindAccessory(this.api, this.log, blind, this.baseUrl, this.timeoutMs),
      ),
    );
  }
}

class SomfyBlindAccessory implements AccessoryPlugin {
  private readonly api: API;
  private readonly log: Logging;
  private readonly led: Led;
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly informationService: Service;
  private readonly service: Service;

  public readonly name: string;
  private position = 100;

  constructor(
    api: API,
    log: Logging,
    blind: BlindConfig,
    baseUrl: string,
    timeoutMs: number,
  ) {
    this.api = api;
    this.log = log;
    this.name = blind.name;
    this.led = blind.led;
    this.baseUrl = baseUrl;
    this.timeoutMs = timeoutMs;

    const { Service: S, Characteristic: C } = api.hap;

    this.informationService = new S.AccessoryInformation()
      .setCharacteristic(C.Manufacturer, 'Somfy')
      .setCharacteristic(C.Model, 'Telis 4 (via Pi GPIO)')
      .setCharacteristic(C.SerialNumber, `somfy-${this.led}`);

    this.service = new S.WindowCovering(this.name);

    this.service
      .getCharacteristic(C.CurrentPosition)
      .onGet(() => this.position);

    this.service
      .getCharacteristic(C.TargetPosition)
      .onGet(() => this.position)
      .onSet((value) => this.handleSetTargetPosition(value));

    this.service
      .getCharacteristic(C.PositionState)
      .onGet(() => C.PositionState.STOPPED);
  }

  getServices(): Service[] {
    return [this.informationService, this.service];
  }

  private async handleSetTargetPosition(value: CharacteristicValue): Promise<void> {
    const { Characteristic: C, HapStatusError, HAPStatus } = this.api.hap;

    const numeric = typeof value === 'number' ? value : Number(value);
    const command = numeric >= 50 ? 'up' : 'down';
    const snapped = numeric >= 50 ? 100 : 0;

    try {
      await this.postCommand({ command, led: this.led });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.log.error(`[${this.name}] ${command} failed: ${message}`);
      throw new HapStatusError(HAPStatus.SERVICE_COMMUNICATION_FAILURE);
    }

    this.position = snapped;
    this.service.getCharacteristic(C.CurrentPosition).updateValue(snapped);
    this.service.getCharacteristic(C.TargetPosition).updateValue(snapped);
  }

  private async postCommand(body: { command: string; led: Led }): Promise<void> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const res = await fetch(`${this.baseUrl}/command`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (!res.ok) {
        const text = await res.text().catch(() => '');
        throw new Error(`HTTP ${res.status}${text ? `: ${text}` : ''}`);
      }
    } finally {
      clearTimeout(timer);
    }
  }
}

export default (api: API): void => {
  api.registerPlatform(PLUGIN_NAME, PLATFORM_NAME, SomfyRemotePlatform);
};
