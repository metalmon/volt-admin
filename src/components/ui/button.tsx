/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

import { cn } from '@/lib/utils'
import { type ButtonHTMLAttributes, forwardRef } from 'react'

export type ButtonVariant = 'primary' | 'secondary'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant
}

/**
 * Fluent-style button, in the two shapes this app needs:
 *
 * - `primary`   — accent-filled (brand raspberry), white text. The
 *   Fluent "positive action" treatment: filled, ~4px radius, a hover state
 *   one step darker than rest, a pressed state one step darker still, and a
 *   visible `:focus-visible` ring in the accent color (keyboard-only, so a
 *   mouse click never flashes a ring).
 * - `secondary` — subtle: neutral surface with a 1px border, brand-tinted
 *   border + text on hover. Used for dismissive/neutral actions that sit
 *   beside a primary one.
 *
 * Both share height, padding, and a minimum width so a row of mixed-variant
 * buttons lines up. Fluent places the primary/positive action rightmost —
 * that's a call-site layout concern (`justify-end`, DOM order), not
 * something this component enforces.
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  ({ variant = 'secondary', className, type = 'button', ...props }, ref) => {
    return (
      <button
        ref={ref}
        type={type}
        className={cn(
          // Shared shape: consistent height/padding/min-width across every
          // button so primary/secondary pairs line up regardless of label length.
          'inline-flex h-9 min-w-20 items-center justify-center gap-2 rounded-[4px] px-4 text-sm font-medium',
          'transition-colors duration-100',
          'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand',
          'disabled:pointer-events-none disabled:opacity-50',
          variant === 'primary' &&
            cn(
              'bg-brand text-brand-foreground',
              'hover:bg-brand-muted',
              'active:brightness-90',
            ),
          variant === 'secondary' &&
            cn(
              'border border-border bg-card text-foreground',
              'hover:border-brand/50 hover:bg-accent hover:text-foreground',
              'active:bg-accent',
            ),
          className,
        )}
        {...props}
      />
    )
  },
)
Button.displayName = 'Button'
