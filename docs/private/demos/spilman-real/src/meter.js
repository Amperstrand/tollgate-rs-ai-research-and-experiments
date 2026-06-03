/**
 * MeterController — Real-time utility meter for Spilman channel consumption.
 * Charlie sells electricity at 5 watts, 1 sat/watt-second.
 * Each second of consumption triggers an auto-payment through the channel.
 */

const WATTS = 5;
const SAT_PER_WATT = 1;
const SAT_PER_SEC = WATTS * SAT_PER_WATT;
const PAY_INTERVAL_SAT = 1;

export class MeterController {
  constructor(opts) {
    this._onPayment = opts.onPayment;
    this._onDepleted = opts.onDepleted;
    this._onStatusChange = opts.onStatusChange;
    this._getChannelState = opts.getChannelState;

    this._isOn = false;
    this._enabled = false;
    this._totalConsumed = 0;
    this._accumulated = 0;
    this._dialAngle = 0;
    this._rafId = null;
    this._lastTickTime = null;
  }

  get isEnabled() { return this._enabled; }
  get isOn() { return this._isOn; }
  get totalConsumed() { return this._totalConsumed; }

  enable() {
    this._enabled = true;
    this._notify();
  }

  disable() {
    this._stop();
    this._enabled = false;
    this._notify();
  }

  toggle() {
    if (!this._enabled) return;
    if (this._isOn) {
      this._stop();
    } else {
      const ch = this._getChannelState();
      const remaining = ch ? ch.capacity - ch.balanceToReceiver : 0;
      if (remaining <= 0) {
        this._onDepleted();
        return;
      }
      this._start();
    }
    this._notify();
  }

  start() {
    if (this._isOn || !this._enabled) return;
    const ch = this._getChannelState();
    const remaining = ch ? ch.capacity - ch.balanceToReceiver : 0;
    if (remaining <= 0) {
      this._onDepleted();
      return;
    }
    this._start();
    this._notify();
  }

  stop() {
    if (!this._isOn) return;
    this._stop();
    this._notify();
  }

  reset() {
    this._stop();
    this._enabled = false;
    this._isOn = false;
    this._totalConsumed = 0;
    this._accumulated = 0;
    this._dialAngle = 0;
    this._notify();
  }

  _start() {
    this._isOn = true;
    this._lastTickTime = performance.now();
    this._tick();
  }

  _stop() {
    this._isOn = false;
    if (this._rafId) {
      cancelAnimationFrame(this._rafId);
      this._rafId = null;
    }
    this._lastTickTime = null;
  }

  _tick() {
    if (!this._isOn) return;

    const now = performance.now();
    const dtSec = Math.min((now - this._lastTickTime) / 1000, 0.25);
    this._lastTickTime = now;

    const consumedThisTick = SAT_PER_SEC * dtSec;
    this._accumulated += consumedThisTick;
    this._dialAngle = (this._dialAngle + consumedThisTick * 36) % 360;

    if (this._accumulated >= PAY_INTERVAL_SAT) {
      const toPay = Math.floor(this._accumulated);

      const ch = this._getChannelState();
      const remaining = ch ? ch.capacity - ch.balanceToReceiver : 0;

      if (remaining < toPay) {
        if (remaining > 0) {
          this._totalConsumed += remaining;
          this._onPayment(remaining);
        }
        this._accumulated = 0;
        this._stop();
        this._onDepleted();
        this._notify();
        return;
      }

      this._totalConsumed += toPay;
      this._accumulated -= toPay;
      this._onPayment(toPay);
    }

    this._notify();
    this._rafId = requestAnimationFrame(() => this._tick());
  }

  _notify() {
    const ch = this._getChannelState();
    const remaining = ch ? ch.capacity - ch.balanceToReceiver : 0;
    this._onStatusChange({
      isOn: this._isOn,
      watts: WATTS,
      satPerSec: SAT_PER_SEC,
      totalConsumed: this._totalConsumed,
      channelRemaining: remaining,
      dialAngle: this._dialAngle,
    });
  }
}
