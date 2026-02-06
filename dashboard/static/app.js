// ============================================================================
// RustQueue Dashboard — Vanilla JS Controller
// Single-page application with Overview, Queues, and Live Events panels.
// ============================================================================

(function () {
    'use strict';

    // ── Configuration ────────────────────────────────────────────────────────

    var REFRESH_INTERVAL_MS = 5000;
    var MAX_EVENTS = 100;
    var API_BASE = '/api/v1';

    // ── State ────────────────────────────────────────────────────────────────

    var state = {
        currentPanel: 'overview',
        events: [],
        ws: null,
        wsConnected: false,
        refreshTimer: null,
        clockTimer: null,
        health: null,
        queues: [],
        error: null,
        dlqSelectedQueue: null,
        dlqJobs: [],
    };

    // ── DOM References ───────────────────────────────────────────────────────

    var dom = {};

    function cacheDom() {
        dom.panels = document.querySelectorAll('.panel');
        dom.navLinks = document.querySelectorAll('.sidebar-nav-link');
        dom.clock = document.getElementById('header-clock');
        dom.version = document.getElementById('header-version');
        dom.statusDot = document.getElementById('status-dot');
        dom.statusText = document.getElementById('status-text');
        dom.uptime = document.getElementById('sidebar-uptime');
        dom.errorBanner = document.getElementById('error-banner');

        // Overview panel
        dom.overviewCards = document.getElementById('overview-cards');
        dom.overviewQueuesCount = document.getElementById('overview-queues-count');

        // Queues panel
        dom.queuesGrid = document.getElementById('queues-grid');

        // DLQ panel
        dom.dlqQueueSelector = document.getElementById('dlq-queue-selector');
        dom.dlqContent = document.getElementById('dlq-content');

        // Events panel
        dom.eventsBody = document.getElementById('events-body');
        dom.eventsConnectionDot = document.getElementById('events-connection-dot');
        dom.eventsConnectionText = document.getElementById('events-connection-text');
        dom.eventsClearBtn = document.getElementById('events-clear-btn');
    }

    // ── Utilities ────────────────────────────────────────────────────────────

    function formatTime(date) {
        return date.toLocaleTimeString('en-US', { hour12: false });
    }

    function formatUptime(seconds) {
        if (seconds < 60) return seconds + 's';
        if (seconds < 3600) return Math.floor(seconds / 60) + 'm ' + (seconds % 60) + 's';
        var h = Math.floor(seconds / 3600);
        var m = Math.floor((seconds % 3600) / 60);
        return h + 'h ' + m + 'm';
    }

    function formatEventTime(isoString) {
        try {
            var d = new Date(isoString);
            return d.toLocaleTimeString('en-US', { hour12: false }) +
                '.' + String(d.getMilliseconds()).padStart(3, '0');
        } catch (e) {
            return isoString;
        }
    }

    function eventTypeCssClass(eventType) {
        return eventType.replace('.', '-');
    }

    // ── Safe DOM Helpers ─────────────────────────────────────────────────────
    // All dynamic content is rendered via safe DOM APIs (createElement,
    // textContent) to prevent XSS. No user-controlled data is ever placed
    // into innerHTML.

    function createEl(tag, className, textContent) {
        var el = document.createElement(tag);
        if (className) el.className = className;
        if (textContent !== undefined) el.textContent = textContent;
        return el;
    }

    // ── API ──────────────────────────────────────────────────────────────────

    function apiFetch(path) {
        return fetch(API_BASE + path)
            .then(function (resp) {
                if (!resp.ok) throw new Error('HTTP ' + resp.status);
                return resp.json();
            });
    }

    function fetchHealth() {
        return apiFetch('/health')
            .then(function (data) {
                state.health = data;
                state.error = null;
                return data;
            })
            .catch(function (err) {
                state.error = 'Failed to reach server: ' + err.message;
                throw err;
            });
    }

    function fetchQueues() {
        return apiFetch('/queues')
            .then(function (data) {
                state.queues = data.queues || [];
                state.error = null;
                return data;
            })
            .catch(function (err) {
                state.error = 'Failed to fetch queues: ' + err.message;
                throw err;
            });
    }

    function fetchDlqJobs(queue) {
        return apiFetch('/queues/' + encodeURIComponent(queue) + '/dlq?limit=50')
            .then(function (data) {
                state.dlqJobs = data.jobs || [];
                return data;
            })
            .catch(function (err) {
                state.dlqJobs = [];
                throw err;
            });
    }

    // ── Rendering: Overview ──────────────────────────────────────────────────

    function aggregateCounts(queues) {
        var totals = { waiting: 0, active: 0, completed: 0, failed: 0, dlq: 0, delayed: 0 };
        for (var i = 0; i < queues.length; i++) {
            var c = queues[i].counts;
            totals.waiting += c.waiting || 0;
            totals.active += c.active || 0;
            totals.completed += c.completed || 0;
            totals.failed += c.failed || 0;
            totals.dlq += c.dlq || 0;
            totals.delayed += c.delayed || 0;
        }
        return totals;
    }

    function renderOverview() {
        var queues = state.queues;
        var counts = aggregateCounts(queues);
        var total = counts.waiting + counts.active + counts.completed +
            counts.failed + counts.dlq + counts.delayed;

        dom.overviewQueuesCount.textContent = queues.length;

        // Clear and rebuild cards using safe DOM methods
        dom.overviewCards.textContent = '';
        dom.overviewCards.appendChild(buildSummaryCard('Total Jobs', total, 'total'));
        dom.overviewCards.appendChild(buildSummaryCard('Waiting', counts.waiting, 'waiting'));
        dom.overviewCards.appendChild(buildSummaryCard('Active', counts.active, 'active'));
        dom.overviewCards.appendChild(buildSummaryCard('Completed', counts.completed, 'completed'));
        dom.overviewCards.appendChild(buildSummaryCard('Failed', counts.failed, 'failed'));
        dom.overviewCards.appendChild(buildSummaryCard('Dead Letter', counts.dlq, 'dlq'));
        dom.overviewCards.appendChild(buildSummaryCard('Delayed', counts.delayed, 'delayed'));
    }

    function buildSummaryCard(label, value, type) {
        var card = createEl('div', 'summary-card ' + type);
        card.appendChild(createEl('div', 'summary-card-label', label));
        card.appendChild(createEl('div', 'summary-card-value', Number(value).toLocaleString()));
        return card;
    }

    // ── Rendering: Queues ────────────────────────────────────────────────────

    function renderQueues() {
        var queues = state.queues;
        dom.queuesGrid.textContent = '';

        if (queues.length === 0) {
            var empty = createEl('div', 'empty-state');
            var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
            svg.setAttribute('viewBox', '0 0 24 24');
            svg.setAttribute('fill', 'none');
            svg.setAttribute('stroke', 'currentColor');
            svg.setAttribute('stroke-width', '2');
            var path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
            path.setAttribute('d', 'M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z');
            svg.appendChild(path);
            empty.appendChild(svg);
            empty.appendChild(createEl('div', 'empty-state-title', 'No queues yet'));
            empty.appendChild(createEl('div', 'empty-state-text', 'Push a job to create your first queue.'));
            dom.queuesGrid.appendChild(empty);
            return;
        }

        for (var i = 0; i < queues.length; i++) {
            dom.queuesGrid.appendChild(buildQueueCard(queues[i]));
        }
    }

    function buildQueueCard(queue) {
        var c = queue.counts;
        var total = (c.waiting || 0) + (c.active || 0) + (c.completed || 0) +
            (c.failed || 0) + (c.dlq || 0) + (c.delayed || 0);

        var card = createEl('div', 'queue-card');

        var header = createEl('div', 'queue-card-header');
        header.appendChild(createEl('div', 'queue-card-name', queue.name));
        header.appendChild(createEl('div', 'queue-card-total', total + ' total'));
        card.appendChild(header);

        var counts = createEl('div', 'queue-card-counts');
        counts.appendChild(buildCountBadge('waiting', 'Waiting', c.waiting));
        counts.appendChild(buildCountBadge('active', 'Active', c.active));
        counts.appendChild(buildCountBadge('delayed', 'Delayed', c.delayed));
        counts.appendChild(buildCountBadge('completed', 'Completed', c.completed));
        counts.appendChild(buildCountBadge('failed', 'Failed', c.failed));
        counts.appendChild(buildCountBadge('dlq', 'DLQ', c.dlq));
        card.appendChild(counts);

        return card;
    }

    function buildCountBadge(type, label, count) {
        var badge = createEl('span', 'count-badge ' + type);
        badge.appendChild(createEl('span', 'badge-dot'));
        badge.appendChild(document.createTextNode(label + ' ' + (count || 0)));
        return badge;
    }

    // ── Rendering: DLQ ────────────────────────────────────────────────────────

    function renderDlqQueueSelector() {
        if (!dom.dlqQueueSelector) return;
        dom.dlqQueueSelector.textContent = '';

        var queues = state.queues;
        if (queues.length === 0) {
            dom.dlqQueueSelector.appendChild(
                createEl('span', 'dlq-no-queues', 'No queues available')
            );
            return;
        }

        for (var i = 0; i < queues.length; i++) {
            var btn = createEl('button', 'dlq-queue-btn', queues[i].name);
            if (state.dlqSelectedQueue === queues[i].name) {
                btn.classList.add('active');
            }
            btn.setAttribute('data-queue', queues[i].name);
            btn.addEventListener('click', function () {
                var queueName = this.getAttribute('data-queue');
                state.dlqSelectedQueue = queueName;
                renderDlqQueueSelector();
                fetchDlqJobs(queueName).then(renderDlqJobs).catch(renderDlqJobs);
            });
            dom.dlqQueueSelector.appendChild(btn);
        }
    }

    function renderDlqJobs() {
        if (!dom.dlqContent) return;
        dom.dlqContent.textContent = '';

        if (!state.dlqSelectedQueue) {
            var empty = createEl('div', 'empty-state');
            empty.appendChild(createEl('div', 'empty-state-title', 'No queue selected'));
            empty.appendChild(createEl('div', 'empty-state-text', 'Select a queue above to view dead-letter jobs.'));
            dom.dlqContent.appendChild(empty);
            return;
        }

        var jobs = state.dlqJobs;
        if (jobs.length === 0) {
            var emptyState = createEl('div', 'empty-state');
            emptyState.appendChild(createEl('div', 'empty-state-title', 'No DLQ jobs'));
            emptyState.appendChild(createEl('div', 'empty-state-text',
                'No dead-letter jobs in queue "' + state.dlqSelectedQueue + '".'));
            dom.dlqContent.appendChild(emptyState);
            return;
        }

        // Build table
        var table = createEl('div', 'dlq-table');

        // Header
        var header = createEl('div', 'dlq-table-header');
        header.appendChild(createEl('div', null, 'Job ID'));
        header.appendChild(createEl('div', null, 'Name'));
        header.appendChild(createEl('div', null, 'Error'));
        header.appendChild(createEl('div', null, 'Attempts'));
        header.appendChild(createEl('div', null, 'Updated At'));
        table.appendChild(header);

        // Rows
        for (var i = 0; i < jobs.length; i++) {
            var job = jobs[i];
            var row = createEl('div', 'dlq-table-row');
            row.appendChild(createEl('div', 'dlq-job-id', (job.id || '').substring(0, 8) + '...'));
            row.appendChild(createEl('div', null, job.name || '-'));
            row.appendChild(createEl('div', 'dlq-error', job.last_error || '-'));
            row.appendChild(createEl('div', null, String(job.attempt || 0) + '/' + String(job.max_attempts || 0)));
            row.appendChild(createEl('div', null, job.updated_at ? formatEventTime(job.updated_at) : '-'));
            table.appendChild(row);
        }

        dom.dlqContent.appendChild(table);
    }

    // ── Rendering: Live Events ───────────────────────────────────────────────

    function buildEventRow(event) {
        var row = createEl('div', 'event-row');

        row.appendChild(createEl('div', 'event-time', formatEventTime(event.timestamp)));
        row.appendChild(createEl('div', 'event-type ' + eventTypeCssClass(event.event), event.event));
        row.appendChild(createEl('div', 'event-queue', event.queue));
        row.appendChild(createEl('div', 'event-id', event.job_id));

        return row;
    }

    function appendEvent(event) {
        state.events.push(event);

        // Trim to max
        while (state.events.length > MAX_EVENTS) {
            state.events.shift();
            if (dom.eventsBody.firstChild) {
                dom.eventsBody.removeChild(dom.eventsBody.firstChild);
            }
        }

        dom.eventsBody.appendChild(buildEventRow(event));

        // Auto-scroll to bottom
        dom.eventsBody.scrollTop = dom.eventsBody.scrollHeight;
    }

    function clearEvents() {
        state.events = [];
        dom.eventsBody.textContent = '';
    }

    // ── WebSocket ────────────────────────────────────────────────────────────

    function connectWebSocket() {
        if (state.ws) {
            state.ws.close();
        }

        var protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        var url = protocol + '//' + window.location.host + API_BASE + '/events';

        try {
            state.ws = new WebSocket(url);
        } catch (e) {
            updateWsStatus(false);
            return;
        }

        state.ws.onopen = function () {
            updateWsStatus(true);
        };

        state.ws.onmessage = function (msg) {
            try {
                var event = JSON.parse(msg.data);
                appendEvent(event);
            } catch (e) {
                // Ignore malformed messages
            }
        };

        state.ws.onclose = function () {
            updateWsStatus(false);
            // Reconnect after 3 seconds
            setTimeout(connectWebSocket, 3000);
        };

        state.ws.onerror = function () {
            updateWsStatus(false);
        };
    }

    function updateWsStatus(connected) {
        state.wsConnected = connected;

        if (dom.eventsConnectionDot) {
            if (connected) {
                dom.eventsConnectionDot.classList.remove('disconnected');
            } else {
                dom.eventsConnectionDot.classList.add('disconnected');
            }
        }

        if (dom.eventsConnectionText) {
            dom.eventsConnectionText.textContent = connected ? 'Connected' : 'Disconnected';
        }

        // Also update header status
        if (dom.statusDot) {
            if (connected) {
                dom.statusDot.classList.remove('disconnected');
            } else {
                dom.statusDot.classList.add('disconnected');
            }
        }
        if (dom.statusText) {
            dom.statusText.textContent = connected ? 'Connected' : 'Disconnected';
        }
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    function navigateTo(panelId) {
        state.currentPanel = panelId;

        // Update nav links
        for (var i = 0; i < dom.navLinks.length; i++) {
            var link = dom.navLinks[i];
            if (link.getAttribute('data-panel') === panelId) {
                link.classList.add('active');
            } else {
                link.classList.remove('active');
            }
        }

        // Update panels
        for (var j = 0; j < dom.panels.length; j++) {
            var panel = dom.panels[j];
            if (panel.id === 'panel-' + panelId) {
                panel.classList.add('active');
            } else {
                panel.classList.remove('active');
            }
        }

        // Refresh data for the new panel
        refreshData();
    }

    // ── Data Refresh ─────────────────────────────────────────────────────────

    function refreshData() {
        var healthPromise = fetchHealth().then(function () {
            renderHealth();
        }).catch(function () {
            renderHealth();
        });

        var queuesPromise = fetchQueues().then(function () {
            renderOverview();
            renderQueues();
            renderDlqQueueSelector();
        }).catch(function () {
            renderOverview();
            renderQueues();
            renderDlqQueueSelector();
        });

        // If DLQ panel is active and a queue is selected, refresh DLQ jobs too.
        if (state.currentPanel === 'dlq' && state.dlqSelectedQueue) {
            fetchDlqJobs(state.dlqSelectedQueue).then(renderDlqJobs).catch(renderDlqJobs);
        }

        Promise.all([healthPromise, queuesPromise]).then(function () {
            renderError();
        }).catch(function () {
            renderError();
        });
    }

    function renderHealth() {
        if (state.health) {
            if (dom.version) {
                dom.version.textContent = 'v' + (state.health.version || '0.0.0');
            }
            if (dom.uptime) {
                dom.uptime.textContent = 'Uptime: ' + formatUptime(state.health.uptime_seconds || 0);
            }
        }
    }

    function renderError() {
        if (state.error && dom.errorBanner) {
            dom.errorBanner.textContent = state.error;
            dom.errorBanner.classList.add('visible');
        } else if (dom.errorBanner) {
            dom.errorBanner.classList.remove('visible');
        }
    }

    // ── Clock ────────────────────────────────────────────────────────────────

    function updateClock() {
        if (dom.clock) {
            dom.clock.textContent = formatTime(new Date());
        }
    }

    // ── Init ─────────────────────────────────────────────────────────────────

    function init() {
        cacheDom();

        // Set up navigation
        for (var i = 0; i < dom.navLinks.length; i++) {
            dom.navLinks[i].addEventListener('click', function (e) {
                e.preventDefault();
                var panelId = this.getAttribute('data-panel');
                if (panelId) navigateTo(panelId);
            });
        }

        // Clear events button
        if (dom.eventsClearBtn) {
            dom.eventsClearBtn.addEventListener('click', clearEvents);
        }

        // Start clock
        updateClock();
        state.clockTimer = setInterval(updateClock, 1000);

        // Initial data load
        refreshData();

        // Auto-refresh
        state.refreshTimer = setInterval(refreshData, REFRESH_INTERVAL_MS);

        // Connect WebSocket
        connectWebSocket();

        // Navigate to default panel
        navigateTo('overview');
    }

    // ── Boot ─────────────────────────────────────────────────────────────────

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

})();
