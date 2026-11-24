import { Button as FoldkitButton } from '@foldkit/ui'
import type { Attribute, Html, HtmlBuilder } from 'foldkit/html'

import { cn } from '@/lib/utils'

/** Button variant keys. Sync with `buttonVariants` is compiler-enforced:
 *  `buttonVariants` is `Record<ButtonVariant, string>` (missing key = error)
 *  and annotated object literals reject unknown keys. */
export const buttonVariantKeys = [
  'default',
  'destructive',
  'outline',
  'secondary',
  'ghost',
  'link',
] as const

// jfa.dev's Button (common/src/components/ui/button.tsx): the same variants, sizes and focus
// ring, so the console's controls match every other jfa.dev app.
export const buttonVariants: Record<ButtonVariant, string> = {
  default: 'bg-primary text-primary-foreground hover:bg-primary/80',
  destructive: 'bg-destructive/10 text-destructive hover:bg-destructive/20 focus-visible:border-destructive/40 focus-visible:ring-destructive/20 dark:bg-destructive/20 dark:hover:bg-destructive/25 dark:focus-visible:ring-destructive/40',
  outline: 'border-border hover:bg-input/50 hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:bg-input/30',
  secondary: 'bg-secondary text-secondary-foreground hover:bg-secondary/80 aria-expanded:bg-secondary aria-expanded:text-secondary-foreground',
  ghost: 'hover:bg-muted hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:hover:bg-muted/50',
  link: 'text-primary underline decoration-dotted underline-offset-4 hover:decoration-solid',
}

export type ButtonVariant = (typeof buttonVariantKeys)[number]

/** Button size keys. Sync with `buttonSizes` is compiler-enforced (see above). */
export const buttonSizeKeys = [
  'default',
  'xs',
  'sm',
  'lg',
  'icon',
  'icon-xs',
  'icon-sm',
  'icon-lg',
] as const

export const buttonSizes: Record<ButtonSize, string> = {
  default: "h-7 gap-1 px-2 text-xs/relaxed has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3.5",
  xs: "h-5 gap-1 rounded-sm px-2 text-xs has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-2.5",
  sm: "h-6 gap-1 px-2 text-xs/relaxed has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3",
  lg: "h-8 gap-1 px-2.5 text-xs/relaxed has-data-[icon=inline-end]:pr-2 has-data-[icon=inline-start]:pl-2 [&_svg:not([class*='size-'])]:size-4",
  icon: "size-7 [&_svg:not([class*='size-'])]:size-3.5",
  'icon-xs': "size-5 rounded-sm [&_svg:not([class*='size-'])]:size-2.5",
  'icon-sm': "size-6 [&_svg:not([class*='size-'])]:size-3",
  'icon-lg': "size-8 [&_svg:not([class*='size-'])]:size-4",
}

export type ButtonSize = (typeof buttonSizeKeys)[number]

export const buttonBase =
  "group/button inline-flex shrink-0 cursor-pointer items-center justify-center rounded-md border border-transparent bg-clip-padding text-xs/relaxed font-medium whitespace-nowrap no-underline transition-all outline-none select-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30 disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"

export type ButtonConfig<M> = Readonly<{
  onClick?: M
  isDisabled?: boolean
  type?: 'button' | 'submit' | 'reset'
  isAutofocus?: boolean
  variant?: ButtonVariant
  size?: ButtonSize
  className?: string
  /** Extra attributes merged onto the button element (ids, handlers,
   *  popoover anchors, …). */
  attributes?: ReadonlyArray<Attribute<M>>
}>

export type ButtonLabel = Html | string | ReadonlyArray<Html | string>

/** Styled button built on the @foldkit/ui Button helper. */
export const button = <M>(config: ButtonConfig<M>, label: ButtonLabel, h: HtmlBuilder<M>): Html =>
  FoldkitButton.view<M>(
    {
      onClick: config.onClick,
      isDisabled: config.isDisabled,
      type: config.type,
      isAutofocus: config.isAutofocus,
      toView: (attributes) =>
        h.button(
          [
            ...attributes.button,
            h.Class(
              cn(
                buttonBase,
                buttonVariants[config.variant ?? 'default'],
                buttonSizes[config.size ?? 'default'],
                config.className,
              ),
            ),
            h.DataAttribute('slot', 'button'),
            ...(config.attributes ?? []),
          ],
          Array.isArray(label) ? label : [label],
        ),
    },
    h,
  )
