// Canonical shadcn Button — track B.3 first drop-in.
//
// Pulled in verbatim from the shadcn/ui catalogue
// (https://ui.shadcn.com/docs/components/button), rewired to use
// the project's `cn` helper at `../../lib/cn`. No other changes.
//
// Variants assume the theme tokens defined in
// `src/tailwind.input.css` (track B.1): `--color-primary`,
// `--color-foreground`, `--color-muted`, `--color-border`,
// `--color-card`, `--color-destructive`. shadcn's default
// `bg-primary text-primary-foreground` pair is mapped to
// `bg-primary text-card` here since the legacy palette doesn't
// carry a separate `primary-foreground` value — `--color-card`
// (off-white in light, near-black in dark) gives the right
// contrast against the orange `--color-primary` accent.
//
// The `asChild` prop lets the consumer render the slot as a
// custom element (e.g. an `<a>` link) while keeping the variant
// styling — same behaviour as upstream shadcn.

import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import * as React from "react";

import { cn } from "../../lib/cn";

const buttonVariants = cva(
  cn(
    "inline-flex items-center justify-center gap-2",
    "whitespace-nowrap rounded text-sm font-medium",
    "transition-colors",
    "focus-visible:outline-2 focus-visible:outline-primary",
    "focus-visible:outline-offset-1",
    "disabled:pointer-events-none disabled:opacity-50",
    "[&_svg]:pointer-events-none [&_svg]:shrink-0",
  ),
  {
    variants: {
      variant: {
        default: "bg-primary text-card hover:opacity-90",
        destructive: "bg-destructive text-card hover:opacity-90",
        outline: cn(
          "border border-border bg-card text-foreground",
          "hover:bg-background hover:border-primary hover:text-primary",
        ),
        secondary: "bg-muted text-card hover:opacity-90",
        ghost: "hover:bg-background hover:text-foreground",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 px-3 text-xs",
        lg: "h-10 px-8",
        icon: "h-8 w-8",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  asChild?: boolean;
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, asChild = false, ...props }, ref) => {
    const Comp = asChild ? Slot : "button";
    return (
      <Comp
        className={cn(buttonVariants({ variant, size, className }))}
        ref={ref}
        {...props}
      />
    );
  },
);
Button.displayName = "Button";

export { Button, buttonVariants };
