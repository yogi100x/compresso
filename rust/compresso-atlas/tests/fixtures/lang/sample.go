// Package jobs schedules background work.
package jobs

import (
	"context"
	"fmt"

	store "example.com/svc/internal/store"
)

import "errors"

const MaxRetries = 3

const (
	defaultName, backupName = "main", "backup"
	_                       = 0
)

var ErrClosed = errors.New("closed")

type Runner interface {
	Run(ctx context.Context) error
}

type Queue[T any] struct {
	items []T
}

type ID = string

type Handler func(ctx context.Context) error

func NewQueue[T any]() *Queue[T] {
	return &Queue[T]{}
}

func (q *Queue[T]) Push(item T) {
	q.items = append(q.items, item)
	q.grow()
}

func (q Queue[T]) grow() {}

func run(ctx context.Context) error {
	const local = 1
	q := NewQueue[int]()
	q.Push(local)
	fmt.Println(len(q.items))
	store.Open(ctx)
	return Handler(run)(ctx)
}
