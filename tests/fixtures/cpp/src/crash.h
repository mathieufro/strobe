#pragma once

namespace crash {

void null_deref();
void abort_signal();
void stack_overflow(int depth);

} // namespace crash
