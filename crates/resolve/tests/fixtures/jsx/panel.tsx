function Panel(props: { label: string }) {
  return <section>{props.label}</section>;
}

// the shadcn/ui convention: declared plainly, exported by a list
export { Panel };
