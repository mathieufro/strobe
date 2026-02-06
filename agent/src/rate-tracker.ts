/**
 * Tracks call rates per function for hot function detection
 */

export interface RateStats {
    funcId: number;
    funcName: string;
    callsLastSecond: number;
    samplingEnabled: boolean;
    sampleRate: number; // 0.0 to 1.0
}

export class RateTracker {
    private readonly HOT_THRESHOLD = 100_000; // calls/sec
    private readonly DEFAULT_SAMPLE_RATE = 0.01; // 1%
    private readonly COOLDOWN_SECONDS = 5;

    // funcId -> call count in current window
    private currentWindow: Map<number, number> = new Map();
    // funcId -> timestamp when sampling was enabled
    private samplingEnabled: Map<number, number> = new Map();
    // funcId -> last rate measurement
    private lastRates: Map<number, number> = new Map();

    private windowStartTime: number = Date.now();

    constructor(
        private readonly funcNames: Map<number, string>,
        private readonly onSamplingChange: (funcId: number, enabled: boolean, rate: number) => void
    ) {
        // Check rates every 100ms
        setInterval(() => this.checkRates(), 100);
    }

    recordCall(funcId: number): boolean {
        const count = (this.currentWindow.get(funcId) || 0) + 1;
        this.currentWindow.set(funcId, count);

        // If sampling is enabled, decide whether to record this call
        if (this.samplingEnabled.has(funcId)) {
            return Math.random() < this.DEFAULT_SAMPLE_RATE;
        }

        return true; // Record all calls when not sampling
    }

    private checkRates(): void {
        const now = Date.now();
        const elapsed = (now - this.windowStartTime) / 1000; // seconds

        if (elapsed < 0.1) return; // Too soon

        // Calculate rates for this window
        for (const [funcId, count] of this.currentWindow.entries()) {
            const rate = count / elapsed;
            this.lastRates.set(funcId, rate);

            const isCurrentlySampling = this.samplingEnabled.has(funcId);

            if (!isCurrentlySampling && rate > this.HOT_THRESHOLD) {
                // Enable sampling
                this.samplingEnabled.set(funcId, now);
                this.onSamplingChange(funcId, true, this.DEFAULT_SAMPLE_RATE);
                // Logging happens in daemon via sampling_state_change message
            }

            if (isCurrentlySampling && rate < this.HOT_THRESHOLD * 0.8) { // 80% threshold for hysteresis
                // Check cooldown period
                const samplingStarted = this.samplingEnabled.get(funcId)!;
                const samplingDuration = (now - samplingStarted) / 1000;

                if (samplingDuration > this.COOLDOWN_SECONDS) {
                    // Disable sampling
                    this.samplingEnabled.delete(funcId);
                    this.onSamplingChange(funcId, false, 1.0);
                    // Logging happens in daemon via sampling_state_change message
                }
            }
        }

        // Reset window
        this.currentWindow.clear();
        this.windowStartTime = now;
    }

    getSamplingStats(): RateStats[] {
        const stats: RateStats[] = [];

        for (const [funcId, rate] of this.lastRates.entries()) {
            const funcName = this.funcNames.get(funcId) || `func_${funcId}`;
            const sampling = this.samplingEnabled.has(funcId);

            stats.push({
                funcId,
                funcName,
                callsLastSecond: Math.round(rate),
                samplingEnabled: sampling,
                sampleRate: sampling ? this.DEFAULT_SAMPLE_RATE : 1.0,
            });
        }

        return stats.filter(s => s.callsLastSecond > 0).sort((a, b) => b.callsLastSecond - a.callsLastSecond);
    }
}
