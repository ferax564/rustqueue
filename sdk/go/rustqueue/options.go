package rustqueue

import (
	"net/http"
	"time"
)

// ClientOption configures a Client.
type ClientOption func(*clientConfig)

type clientConfig struct {
	token      string
	timeout    time.Duration
	httpClient *http.Client
}

func defaultClientConfig() clientConfig {
	return clientConfig{
		timeout: 30 * time.Second,
	}
}

// WithToken sets the bearer token for authentication.
func WithToken(token string) ClientOption {
	return func(c *clientConfig) {
		c.token = token
	}
}

// WithTimeout sets the HTTP request timeout. Default is 30 seconds.
func WithTimeout(timeout time.Duration) ClientOption {
	return func(c *clientConfig) {
		c.timeout = timeout
	}
}

// WithHTTPClient sets a custom http.Client. When provided, the timeout
// option is ignored (the caller is responsible for configuring the client's timeout).
func WithHTTPClient(client *http.Client) ClientOption {
	return func(c *clientConfig) {
		c.httpClient = client
	}
}

// TcpClientOption configures a TcpClient.
type TcpClientOption func(*tcpClientConfig)

type tcpClientConfig struct {
	token                string
	autoReconnect        bool
	maxReconnectAttempts int
	reconnectDelayMs     int
}

func defaultTcpClientConfig() tcpClientConfig {
	return tcpClientConfig{
		autoReconnect:        true,
		maxReconnectAttempts: 10,
		reconnectDelayMs:     1000,
	}
}

// WithTcpToken sets the bearer token for TCP authentication.
func WithTcpToken(token string) TcpClientOption {
	return func(c *tcpClientConfig) {
		c.token = token
	}
}

// WithAutoReconnect enables or disables automatic reconnection. Default is true.
func WithAutoReconnect(enabled bool) TcpClientOption {
	return func(c *tcpClientConfig) {
		c.autoReconnect = enabled
	}
}

// WithMaxReconnectAttempts sets the maximum number of reconnection attempts. Default is 10.
func WithMaxReconnectAttempts(max int) TcpClientOption {
	return func(c *tcpClientConfig) {
		c.maxReconnectAttempts = max
	}
}

// WithReconnectDelay sets the base delay between reconnection attempts in milliseconds.
// Actual delay uses exponential backoff: delay * 2^attempt. Default is 1000ms.
func WithReconnectDelay(delayMs int) TcpClientOption {
	return func(c *tcpClientConfig) {
		c.reconnectDelayMs = delayMs
	}
}
