package rustqueue

import "fmt"

// RustQueueError represents an error returned by the RustQueue server
// or a client-side error (network, parse, etc.).
type RustQueueError struct {
	// Code is a machine-readable error code (e.g. "NOT_FOUND", "VALIDATION_ERROR").
	Code string
	// Message is a human-readable error message.
	Message string
	// StatusCode is the HTTP status code, if the error originated from an HTTP response.
	// Zero for non-HTTP errors.
	StatusCode int
}

// Error implements the error interface.
func (e *RustQueueError) Error() string {
	if e.StatusCode > 0 {
		return fmt.Sprintf("RustQueueError [%s] (HTTP %d): %s", e.Code, e.StatusCode, e.Message)
	}
	return fmt.Sprintf("RustQueueError [%s]: %s", e.Code, e.Message)
}

// IsNotFound returns true if the error is a NOT_FOUND error.
func (e *RustQueueError) IsNotFound() bool {
	return e.Code == "NOT_FOUND"
}

// IsRustQueueError checks whether an error is a *RustQueueError and returns it.
func IsRustQueueError(err error) (*RustQueueError, bool) {
	if rqe, ok := err.(*RustQueueError); ok {
		return rqe, true
	}
	return nil, false
}
