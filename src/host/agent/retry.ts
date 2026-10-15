/** When and how long a run waits before it asks the model again after a request failed for a passing reason. */
import { isRetryableAssistantError, type AssistantMessage } from '@earendil-works/pi-ai'

/** Automatic retries of one run before it gives up and leaves the retry to the user. */
export const AUTO_RETRIES = 3
/** Wait before the first automatic retry; each further one waits twice as long. */
export const RETRY_BASE_MS = 5_000
/** Longest wait the run accepts, including one the provider asks for; a longer request is left to the user. */
export const RETRY_MAX_MS = 120_000

/** Statuses that say the request itself is wrong, so sending it again gives the same answer. */
const PERMANENT_STATUS = /^(?:error:\s*)?(4\d\d)\b/i
const PASSING_4XX = new Set([408, 409, 425, 429])
/** Our own timeout notice and Chinese provider wording the SDK's English patterns miss. */
const PASSING_TEXT = /超时|繁忙|稍后再?试|限流|过载/

/**
 * Whether a failed reply is worth requesting again unchanged: overload, rate limits, dropped connections, timeouts.
 * Quota, billing, authentication and malformed requests are not.
 * @param message The failed reply.
 * @returns Whether to retry.
 */
export function isPassingFailure(message: AssistantMessage): boolean {
  if (message.stopReason !== 'error') return false
  const text = message.errorMessage ?? ''
  const status = PERMANENT_STATUS.exec(text.trim())
  if (status !== null && !PASSING_4XX.has(Number(status[1]))) return false
  return isRetryableAssistantError(message) || PASSING_TEXT.test(text)
}

/**
 * Wait before an automatic retry: the provider's requested delay when the error carries one, else exponential backoff
 * from {@link RETRY_BASE_MS} with ±15 % jitter so chats that failed together do not retry together.
 * @param attempt 1 for the first retry.
 * @param errorText The provider's error text.
 * @param random Source of jitter in [0, 1).
 * @returns Milliseconds to wait, or `undefined` when the provider asks for longer than {@link RETRY_MAX_MS}.
 */
export function retryDelay(attempt: number, errorText: string, random: () => number = Math.random): number | undefined {
  const requested = /requested (\d+(?:\.\d+)?)s retry delay|retry[- ]after[^\d]{0,12}(\d+(?:\.\d+)?)\s*(?:s\b|sec|second)/i.exec(errorText)
  if (requested !== null) {
    const ms = Number(requested[1] ?? requested[2]) * 1000
    if (ms > RETRY_MAX_MS) return undefined
    return Math.max(ms, RETRY_BASE_MS)
  }
  const backoff = RETRY_BASE_MS * 2 ** Math.max(0, attempt - 1)
  return Math.round(Math.min(backoff, RETRY_MAX_MS) * (0.85 + random() * 0.3))
}
