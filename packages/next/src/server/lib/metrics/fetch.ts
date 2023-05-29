import type { StaticGenerationStore } from '../../../client/components/static-generation-async-storage'

/**
 * Describes if this specific fetch was served from the cache (a `hit`) or not
 * (a `miss`).
 */
type FetchCacheStatus = 'hit' | 'miss' | 'bypass'

/**
 * Describes a specific fetch that a user performed. This records timings and
 * cache hit statuses for display during development.
 */
export type FetchEvent = {
  /**
   * The ID of the fetch event. Currently an incrementing value for each fetch
   * event recorded.
   */
  id: number

  /**
   * The URL that was fetched.
   */
  url: string

  /**
   * The time (in ms) that his fetch even was started at.
   */
  start: number

  /**
   * The time (in ms) that this fetch event was completed at.
   */
  end: number

  /**
   * The HTTP method that was used for this fetch event.
   */
  method: string

  /**
   * The returned status code for this fetch event.
   */
  status: number

  /**
   * The cache information for this fetch event.
   */
  cache: {
    /**
     * The cache hit status for this fetch event.
     */
    status: FetchCacheStatus

    /**
     * The reason tha this cache status status was provided.
     */
    reason?: string
  }
}

/**
 * Records a fetch event (if we haven't already processed it) in the static
 * generation store.
 *
 * @param store the store to add the fetch event in
 * @param event the fetch event to record
 * @returns true if the event was recorded, false if it was a duplicate
 */
export function recordFetchEvent(
  store: StaticGenerationStore,
  event: Omit<FetchEvent, 'end'>
) {
  // Ensure that the metrics are initialized.
  store.fetchMetrics ??= []

  // If there is already an event recorded for the same url/status/method, don't
  // record the event again. These requests are de-duped by React automatically
  // and it would result in confusing logs to the user (seemingly indicating
  // that there was multiple requests made to the origin).
  for (const { url, status, method } of store.fetchMetrics) {
    if (
      event.url !== url ||
      event.status !== status ||
      event.method !== method
    ) {
      continue
    }

    return false
  }

  // Push the event into the array.
  store.fetchMetrics.push({ ...event, end: Date.now() })

  return true
}
