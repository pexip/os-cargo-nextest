// SPDX-License-Identifier: Apache-2.0 OR MIT

use pin_project::pin_project;

// The same implementation.

#[pin_project] //~ ERROR E0119
struct Foo<T, U> {
    #[pin]
    f1: T,
    f2: U,
}

// conflicting implementations
impl<T, U> Unpin for Foo<T, U> where T: Unpin {} // Conditional Unpin impl

// The implementation that under different conditions.

#[pin_project] //~ ERROR E0119
struct Bar<T, U> {
    #[pin]
    f1: T,
    f2: U,
}

// conflicting implementations
impl<T, U> Unpin for Bar<T, U> {} // Non-conditional Unpin impl

#[pin_project] //~ ERROR E0119
struct Baz<T, U> {
    #[pin]
    f1: T,
    f2: U,
}

// conflicting implementations
impl<T: Unpin, U: Unpin> Unpin for Baz<T, U> {} // Conditional Unpin impl

fn main() {}
