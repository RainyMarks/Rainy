/** Owns one cancellable computation and one retained result per tool instance. */
(function (scope) {
    const AUTO_BYTES = 65536;
    const DEBOUNCE_MS = 160;

    /** Counts enough UTF-8 bytes to decide whether automatic work is permitted. */
    function permitsAutomatic(text) {
        let bytes = 0;
        for (const character of text) {
            const code = character.codePointAt(0);
            bytes += code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4;
            if (bytes > AUTO_BYTES) return false;
        }
        return true;
    }

    class ComputeCoordinator {
        constructor(options = {}) {
            this.createWorker = options.createWorker || (() => new Worker('js/workers/compute.js'));
            this.setTimer = options.setTimer || ((callback, delay) => scope.setTimeout(callback, delay));
            this.clearTimer = options.clearTimer || (timer => scope.clearTimeout(timer));
            this.worker = null;
            this.pending = null;
            this.timer = null;
            this.sequence = 0;
            this.cached = null;
            this.disposed = false;
        }

        /** Cancels queued work and terminates executing synchronous worker code. */
        cancel(release = false) {
            if (this.timer !== null) this.clearTimer(this.timer);
            this.timer = null;
            const pending = this.pending;
            this.pending = null;
            if (pending || release) {
                if (this.worker) this.worker.terminate();
                this.worker = null;
                this.cached = null;
            }
            if (pending) pending.resolve(null);
        }

        /** Resolves cancelled or automatically deferred requests with null. */
        run(payload, options = {}) {
            if (this.disposed) return Promise.resolve(null);
            const key = JSON.stringify(payload);
            if (this.pending && this.pending.key === key && !options.force) return this.pending.promise;
            this.cancel();
            if (options.automatic && !permitsAutomatic(payload.input || '')) return Promise.resolve(null);
            if (this.cached && this.cached.key === key && !options.force) return Promise.resolve(this.cached.result);
            const id = ++this.sequence;
            let resolve, reject;
            const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
            const pending = { id, key, promise, resolve, reject };
            this.pending = pending;
            const start = () => {
                this.timer = null;
                if (this.pending !== pending) return;
                try {
                    if (!this.worker) this.worker = this.createWorker();
                    const worker = this.worker;
                    worker.onmessage = event => {
                        if (this.worker !== worker || this.pending !== pending || event.data.id !== id) return;
                        this.pending = null;
                        if (event.data.error) {
                            reject(new Error(event.data.error));
                        } else {
                            this.cached = { key, result: event.data.result };
                            resolve(event.data.result);
                        }
                    };
                    worker.onerror = event => {
                        if (this.worker !== worker || this.pending !== pending) return;
                        this.pending = null;
                        worker.terminate();
                        this.worker = null;
                        this.cached = null;
                        reject(new Error(event.message || '计算服务加载失败，请重试。'));
                    };
                    worker.postMessage({ id, ...payload });
                } catch (error) {
                    this.pending = null;
                    if (this.worker) this.worker.terminate();
                    this.worker = null;
                    reject(error);
                }
            };
            if (options.automatic) {
                try { this.timer = this.setTimer(start, DEBOUNCE_MS); }
                catch (error) { this.pending = null; reject(error); }
            } else start();
            return promise;
        }

        /** Releases retained worker data; a disposed coordinator cannot restart. */
        dispose() { this.disposed = true; this.cancel(true); }
    }

    scope.IceSkyComputeCoordinator = ComputeCoordinator;
    scope.IceSkyPermitsAutomatic = permitsAutomatic;
})(globalThis);
