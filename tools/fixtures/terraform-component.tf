# Unicode and template bodies keep exact source coordinates: 文😀
variable "message" {
  type    = string
  default = "world"
}

locals {
  label = "hello ${upper(var.message)}"
  body = <<-EOT
    literal } and ${var.message}
  EOT
}
