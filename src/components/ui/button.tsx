import { cva, type VariantProps } from "class-variance-authority";
import type { ButtonHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 rounded-full px-4 py-2 text-[13px] font-medium transition disabled:cursor-not-allowed disabled:opacity-50",
  {
    variants: {
      variant: {
        primary:
          "bg-ember text-white shadow-[0_8px_20px_rgba(180,83,9,0.22)] hover:bg-ember-dark",
        quiet: "border border-line bg-card text-ink hover:bg-sidebar",
        danger: "border border-line bg-card text-ember-dark hover:bg-clay",
        soft: "bg-moss text-pine hover:bg-moss/80",
      },
      size: {
        md: "px-4 py-2",
        sm: "px-3 py-1.5 text-xs",
      },
    },
    defaultVariants: {
      variant: "primary",
      size: "md",
    },
  },
);

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & VariantProps<typeof buttonVariants>;

export function Button({ className, variant, size, type = "button", ...props }: ButtonProps) {
  return <button className={cn(buttonVariants({ variant, size }), className)} type={type} {...props} />;
}
